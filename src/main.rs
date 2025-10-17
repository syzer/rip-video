use std::{
    env,
    error::Error,
    fs,
    io::{self, BufRead, BufReader},
    process::{Command, Stdio},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, LineGauge, Paragraph, Wrap},
};

mod banner;

use banner::Banner;

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
enum Stage {
    Download,
    Split,
    Transcribe,
}

enum WorkerMessage {
    // Legacy download progress
    Progress { ratio: f64, eta: Option<String> },
    // Stage-aware progress for multi-phase pipeline
    StageProgress { stage: Stage, ratio: f64, eta: Option<String>, note: Option<String> },
    Status(Result<(), String>),
    DownloadLog(String),
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
    let output_target = resolve_ytdlp_output();
    let has_valid_link = is_valid_url(&download_url);

    let mut progress = 0.0_f64; // download progress
    let mut progress_synced = false;
    let mut split_progress: f64 = 0.0;
    let mut split_note: Option<String> = None;
    let mut trans_progress: f64 = 0.0;
    let mut trans_note: Option<String> = None;
    let mut last_tick = Instant::now();
    let tick_rate = Duration::from_millis(120);
    let mut download_status = DownloadStatus::Idle;
    let mut download_error: Option<String> = None;
    let mut download_rx: Option<mpsc::Receiver<WorkerMessage>> = None;
    let mut worker_cancel: Option<Arc<AtomicBool>> = None;
    let mut debug_lines: Vec<String> = Vec::new();
    let mut eta_text: Option<String> = None;

    loop {
        if has_valid_link {
            if matches!(download_status, DownloadStatus::Idle) {
                let (tx, rx) = mpsc::channel();
                let cancel_flag = Arc::new(AtomicBool::new(false));
                let cancel_for_thread = Arc::clone(&cancel_flag);

                let download_link = download_url.clone();
                let output_target_clone = output_target.clone();
                thread::spawn(move || {
                    download_worker(download_link, output_target_clone, tx, cancel_for_thread)
                });

                download_rx = Some(rx);
                download_status = DownloadStatus::Running;
                progress = 0.0;
                progress_synced = false;
                last_tick = Instant::now();
                worker_cancel = Some(cancel_flag);
                debug_lines.clear();
                eta_text = None;
            }

            if let Some(rx) = download_rx.as_ref() {
                let mut messages = Vec::new();
                let mut disconnected = false;
                loop {
                    match rx.try_recv() {
                        Ok(message) => messages.push(message),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }

                for message in messages {
                    match message {
                        WorkerMessage::Progress { ratio, eta } => {
                            progress = ratio.clamp(0.0, 1.0);
                            progress_synced = true;
                            eta_text = eta;
                        }
                        WorkerMessage::StageProgress { stage, ratio, eta, note } => {
                            match stage {
                                Stage::Download => {
                                    progress = ratio.clamp(0.0, 1.0);
                                    progress_synced = true;
                                    eta_text = eta;
                                }
                                Stage::Split => {
                                    split_progress = ratio.clamp(0.0, 1.0);
                                    split_note = note;
                                }
                                Stage::Transcribe => {
                                    trans_progress = ratio.clamp(0.0, 1.0);
                                    trans_note = note;
                                }
                            }
                        }
                        WorkerMessage::Status(result) => {
                            download_rx = None;
                            worker_cancel = None;
                            match result {
                                Ok(()) => {
                                    progress = 1.0;
                                    split_progress = 1.0;
                                    trans_progress = 1.0;
                                    download_status = DownloadStatus::Success;
                                    download_error = None;
                                    eta_text = None;
                                }
                                Err(err) => {
                                    progress = 0.0;
                                    split_progress = 0.0;
                                    trans_progress = 0.0;
                                    download_status = DownloadStatus::Failed;
                                    download_error = Some(err);
                                    eta_text = None;
                                }
                            }
                        }
                        WorkerMessage::DownloadLog(line) => {
                            debug_lines.push(line);
                            if debug_lines.len() > DEBUG_MAX_LINES {
                                let overflow = debug_lines.len() - DEBUG_MAX_LINES;
                                debug_lines.drain(0..overflow);
                            }
                        }
                    }
                }

                if disconnected && matches!(download_status, DownloadStatus::Running) {
                    download_rx = None;
                    worker_cancel = None;
                    download_status = DownloadStatus::Failed;
                    download_error = Some("downloader process disconnected".to_string());
                    eta_text = None;
                }
            }

            if matches!(download_status, DownloadStatus::Running)
                && !progress_synced
                && last_tick.elapsed() >= tick_rate
            {
                progress = (progress + 0.02) % 1.0;
                last_tick = Instant::now();
            }
        }

        terminal.draw(|frame| {
            let area = frame.size();
            let centered = Layout::default()
                .direction(Direction::Vertical)
                .margin(2)
                .constraints([Constraint::Min(GLYPH_HEIGHT as u16 + 6)].as_ref())
                .split(area);

            let block = Block::default().title("Message").borders(Borders::ALL);
            let inner = block.inner(centered[0]);
            frame.render_widget(block, centered[0]);

            let mut status_lines: Vec<(String, Color)> = Vec::new();
            match (&download_status, has_valid_link) {
                (DownloadStatus::Idle, true) => {
                    status_lines.push((format!("ready: yt-dlp -> {}", output_target), Color::Gray))
                }
                (DownloadStatus::Running, true) => status_lines.push((
                    format!("downloader: yt-dlp running -> {output_target}"),
                    Color::Cyan,
                )),
                (DownloadStatus::Success, true) => status_lines.push((
                    format!("download complete -> {output_target}"),
                    Color::LightGreen,
                )),
                (DownloadStatus::Failed, true) => status_lines.push((
                    format!(
                        "download failed -> {}: {}",
                        output_target,
                        download_error.as_deref().unwrap_or("unknown error")
                    ),
                    Color::Red,
                )),
                _ => {}
            }

            if matches!(download_status, DownloadStatus::Running) {
                if let Some(eta) = eta_text.as_ref() {
                    status_lines.push((
                        format!("progress: {:.1}% ETA {eta}", progress * 100.0),
                        Color::Cyan,
                    ));
                } else {
                    status_lines.push((format!("progress: {:.1}%", progress * 100.0), Color::Cyan));
                }
            }

            let status_height = status_lines.len().max(1) as u16;
                let constraints: Vec<Constraint> = if has_valid_link {
                    vec![
                        Constraint::Length(GLYPH_HEIGHT as u16),
                        Constraint::Length(status_height),
                        Constraint::Length(3),
                        Constraint::Min(GLYPH_HEIGHT as u16),
                    ]
                } else {
                    vec![
                        Constraint::Length(GLYPH_HEIGHT as u16),
                        Constraint::Length(status_height),
                        Constraint::Min(GLYPH_HEIGHT as u16),
                    ]
                };

            let sections = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(inner);

            let header = Banner::new("rip Video")
                .style(Style::new().fg(Color::Yellow))
                .spacing(0);
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
                        Constraint::Length(1),
                        Constraint::Length(1),
                        Constraint::Length(1),
                    ])
                    .split(gauge_area);

                // Download gauge
                let dl_label = if let Some(eta) = eta_text.as_ref() {
                    format!("dl: {:.0}% ETA {}", (progress * 100.0).min(100.0), eta)
                } else {
                    format!("dl: {:.0}%", (progress * 100.0).min(100.0))
                };
                let dl_gauge = LineGauge::default()
                    .label(dl_label)
                    .gauge_style(Style::default().fg(Color::LightGreen))
                    .ratio(progress.min(1.0));
                frame.render_widget(dl_gauge, gauge_rows[0]);

                // Split gauge
                let split_label = if let Some(note) = split_note.as_ref() {
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
                let trans_label = if let Some(note) = trans_note.as_ref() {
                    format!(
                        "transcribe: {:.0}% ({})",
                        (trans_progress * 100.0).min(100.0),
                        note
                    )
                } else {
                    format!(
                        "transcribe: {:.0}%",
                        (trans_progress * 100.0).min(100.0)
                    )
                };
                let trans_gauge = LineGauge::default()
                    .label(trans_label)
                    .gauge_style(Style::default().fg(Color::Cyan))
                    .ratio(trans_progress.min(1.0));
                frame.render_widget(trans_gauge, gauge_rows[2]);

                let debug_present = !debug_lines.is_empty();

                let link_display =
                    chunk_text_to_width(display_url.as_str(), (body_area.width).max(1) as usize);
                let link_line_count = link_display.lines().count().max(1) as u16;

                let mut body_constraints = vec![Constraint::Length(link_line_count)];
                if debug_present {
                    let debug_height =
                        (debug_lines.len().max(1) as u16).min(DEBUG_MAX_LINES as u16) + 2;
                    body_constraints.push(Constraint::Length(debug_height));
                }

                let split = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints(body_constraints)
                    .split(body_area);

                let mut index = 0;
                let link_area = split[index];
                index += 1;

                let link_para = Paragraph::new(link_display)
                    .alignment(Alignment::Left)
                    .style(Style::new().fg(Color::White));
                frame.render_widget(link_para, link_area);

                if debug_present {
                    let debug_area = split[index];
                    let debug_block = Block::default().title("Debug").borders(Borders::ALL);
                    let debug_text = debug_lines.join("\n");
                    let debug_para = Paragraph::new(debug_text)
                        .wrap(Wrap { trim: true })
                        .block(debug_block)
                        .alignment(Alignment::Left)
                        .style(Style::new().fg(Color::DarkGray));
                    frame.render_widget(debug_para, debug_area);
                }
            } else {
                let body_area = sections[2];
                let hint = Paragraph::new(
                    "go to chrome terminal, find a url for `videomanifest`, and copy a URL",
                )
                .alignment(Alignment::Center)
                .style(Style::new().fg(Color::Red));
                frame.render_widget(hint, body_area);
            }
        })?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press
                    && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter)
                {
                    if let Some(cancel) = worker_cancel.take() {
                        cancel.store(true, Ordering::SeqCst);
                    }
                    break;
                }
            }
        }
    }

    Ok(())
}

fn sanitize_clipboard(input: &str) -> String {
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

fn download_worker(
    link: String,
    output_file: String,
    tx: mpsc::Sender<WorkerMessage>,
    cancel: Arc<AtomicBool>,
) {
    let _ = tx.send(WorkerMessage::DownloadLog(format!("link: {link}")));
    let sanitized_link = sanitize_clipboard(&link);
    if sanitized_link != link {
        let _ = tx.send(WorkerMessage::DownloadLog(format!(
            "link sanitized: {sanitized_link}"
        )));
    }

    // Step 1: download audio only with the exact flags provided
    let args: Vec<String> = vec![
        "-f".into(),
        "bestaudio".into(),
        "-o".into(),
        output_file.clone(),
        "-N".into(),
        "4".into(),
        "--concurrent-fragments".into(),
        "4".into(),
        "--fragment-retries".into(),
        "50".into(),
        "--retries".into(),
        "50".into(),
        "--retry-sleep".into(),
        "fragment:5".into(),
        "--limit-rate".into(),
        "4M".into(),
        "--force-ipv4".into(),
        "--newline".into(),
        sanitized_link.clone(),
    ];

    let _ = tx.send(WorkerMessage::DownloadLog(format!(
        "yt-dlp command: yt-dlp {}",
        args.join(" ")
    )));
    let _ = tx.send(WorkerMessage::DownloadLog(format!(
        "output file: {}",
        output_file
    )));
    let _ = tx.send(WorkerMessage::Progress {
        ratio: 0.0,
        eta: None,
    });

    let mut command = Command::new("yt-dlp");
    command.args(&args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            let _ = tx.send(WorkerMessage::Status(Err(err.to_string())));
            return;
        }
    };

    let spawn_reader = |stream: Option<std::process::ChildStdout>,
                        tx: mpsc::Sender<WorkerMessage>,
                        cancel: Arc<AtomicBool>| {
        if let Some(stream) = stream {
            let log_tx = tx.clone();
            let progress_tx = tx;
            thread::spawn(move || {
                let reader = BufReader::new(stream);
                for line_result in reader.lines() {
                    if cancel.load(Ordering::SeqCst) {
                        break;
                    }
                    let line = match line_result {
                        Ok(line) => line,
                        Err(_) => break,
                    };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let _ = log_tx.send(WorkerMessage::DownloadLog(line.clone()));
                    if let Some((ratio, eta)) = parse_ytdlp_progress(&line) {
                        let _ = progress_tx.send(WorkerMessage::StageProgress {
                            stage: Stage::Download,
                            ratio,
                            eta,
                            note: None,
                        });
                    }
                }
            });
        }
    };

    let stdout_cancel = Arc::clone(&cancel);
    spawn_reader(child.stdout.take(), tx.clone(), stdout_cancel);
    let stderr_cancel = Arc::clone(&cancel);
    if let Some(stderr) = child.stderr.take() {
        let log_tx = tx.clone();
        let progress_tx = tx.clone();
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line_result in reader.lines() {
                if stderr_cancel.load(Ordering::SeqCst) {
                    break;
                }
                let line = match line_result {
                    Ok(line) => line,
                    Err(_) => break,
                };
                if line.trim().is_empty() {
                    continue;
                }
                let _ = log_tx.send(WorkerMessage::DownloadLog(line.clone()));
                if let Some((ratio, eta)) = parse_ytdlp_progress(&line) {
                    let _ = progress_tx.send(WorkerMessage::StageProgress {
                        stage: Stage::Download,
                        ratio,
                        eta,
                        note: None,
                    });
                }
            }
        });
    }

    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    // Step 1 complete; now split audio into 10 parts
                    match split_audio_to_parts(&output_file, "parts10", &tx, &cancel) {
                        Ok(()) => {
                            // Transcribe the parts concurrently (10 workers)
                            match transcribe_parts_10("parts10", &tx, &cancel) {
                                Ok(()) => {
                                    let _ = tx.send(WorkerMessage::Status(Ok(())));
                                }
                                Err(err) => {
                                    let _ = tx.send(WorkerMessage::Status(Err(err)));
                                }
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(WorkerMessage::Status(Err(err)));
                        }
                    }
                } else {
                    let code = status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "terminated by signal".to_string());
                    let _ = tx.send(WorkerMessage::DownloadLog(format!(
                        "yt-dlp exited with {code}"
                    )));
                    let _ = tx.send(WorkerMessage::Status(Err(format!("exit {code}"))));
                }
                break;
            }
            Ok(None) => {}
            Err(err) => {
                let _ = tx.send(WorkerMessage::Status(Err(err.to_string())));
                break;
            }
        }

        thread::sleep(Duration::from_millis(100));
    }
}
fn chunk_text_to_width(text: &str, width: usize) -> String {
    let width = width.max(1);
    let mut output = String::new();
    let mut current = 0;
    for ch in text.chars() {
        if current >= width {
            output.push('\n');
            current = 0;
        }
        output.push(ch);
        current += 1;
    }
    output
}

fn parse_ytdlp_progress(line: &str) -> Option<(f64, Option<String>)> {
    if !line.starts_with("[download]") || !line.contains('%') {
        return None;
    }

    let mut parts = line.split_whitespace();
    let _ = parts.next(); // [download]
    let percent_token = parts.next()?;
    let percent_str = percent_token.trim_end_matches('%');
    let percent = percent_str.parse::<f64>().ok()?;

    let eta = if let Some(idx) = line.find("ETA") {
        let eta_part = line[idx + 3..].trim();
        let eta_token = eta_part.split_whitespace().next().unwrap_or("");
        if eta_token.is_empty() {
            None
        } else {
            Some(eta_token.to_string())
        }
    } else {
        None
    };

    Some((percent / 100.0, eta))
}

fn resolve_ytdlp_output() -> String {
    // Default to audio filename for Step 1; override with YTDLP_OUTPUT if desired
    env::var("YTDLP_OUTPUT").unwrap_or_else(|_| "audio.m4a".to_string())
}

fn split_audio_to_parts(
    input_path: &str,
    parts_dir: &str,
    tx: &mpsc::Sender<WorkerMessage>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    // Remove and recreate the output directory (like `rm -rf parts10 && mkdir parts10`)
    let _ = fs::remove_dir_all(parts_dir);
    fs::create_dir_all(parts_dir).map_err(|e| e.to_string())?;

    // Probe duration in seconds
    let dur = probe_duration_seconds(input_path)?;
    if dur <= 0.0 {
        return Err("could not determine audio duration".to_string());
    }

    let sr: f64 = 16000.0;
    let parts = 10u32;
    let step = dur / parts as f64; // seconds per part

    let _ = tx.send(WorkerMessage::DownloadLog(format!(
        "splitting into {parts} parts; duration={:.3}s; step={:.3}s; sr=16000",
        dur, step
    )));
    let _ = tx.send(WorkerMessage::StageProgress {
        stage: Stage::Split,
        ratio: 0.0,
        eta: None,
        note: Some(format!("0/{}", parts)),
    });

    for i in 0..parts {
        if cancel.load(Ordering::SeqCst) {
            return Err("canceled".to_string());
        }

        let s = i as f64 * step;
        let remaining = (dur - s).max(0.0);
        let len = remaining.min(step);
        if len <= 0.0 {
            break;
        }

        // Compute sample-precise boundaries
        let start_sample = (s * sr).round() as i64;
        let end_sample = ((s + len) * sr).round() as i64 - 1;
        let out = format!("{}/{:03}.wav", parts_dir, i);

        let af = format!(
            "atrim=start_sample={}:end_sample={},asetpts=N/SR/TB",
            start_sample, end_sample
        );

        let ff_args: Vec<String> = vec![
            "-y".into(),
            "-loglevel".into(),
            "error".into(),
            "-i".into(),
            input_path.to_string(),
            "-af".into(),
            af,
            "-ac".into(),
            "1".into(),
            "-ar".into(),
            "16000".into(),
            "-c:a".into(),
            "pcm_s16le".into(),
            out.clone(),
        ];

        let _ = tx.send(WorkerMessage::DownloadLog(format!(
            "ffmpeg[{}]: ffmpeg {}",
            i,
            ff_args.join(" ")
        )));

        let mut child = Command::new("ffmpeg")
            .args(&ff_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;

        loop {
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        let code = status
                            .code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "terminated by signal".to_string());
                        return Err(format!(
                            "ffmpeg exited with {code} on part {}",
                            i
                        ));
                    }
                    break;
                }
                Ok(None) => {}
                Err(err) => return Err(err.to_string()),
            }
            thread::sleep(Duration::from_millis(100));
        }
        let done = (i + 1) as f64;
        let ratio = done / parts as f64;
        let _ = tx.send(WorkerMessage::StageProgress {
            stage: Stage::Split,
            ratio,
            eta: None,
            note: Some(format!("{}/{}", i + 1, parts)),
        });
    }

    let _ = tx.send(WorkerMessage::DownloadLog("split into 10 parts complete".to_string()));
    Ok(())
}

fn probe_duration_seconds(input_path: &str) -> Result<f64, String> {
    let args = [
        "-v",
        "error",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
        input_path,
    ];

    let output = Command::new("ffprobe")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err("ffprobe failed".to_string());
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let dur: f64 = stdout
        .parse()
        .map_err(|_| format!("invalid duration from ffprobe: {}", stdout))?;
    Ok(dur)
}

fn transcribe_parts_10(
    parts_dir: &str,
    tx: &mpsc::Sender<WorkerMessage>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    // Resolve binaries and params from env, with sensible defaults
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    let ffmpeg_bin = env::var("FFMPEG_WHISPER_BIN")
        .unwrap_or_else(|_| format!("{}/.local/ffmpeg-whisper/bin/ffmpeg", home));
    let model_path = env::var("WHISPER_MODEL_PATH").unwrap_or_else(|_| {
        format!("{}/whisper.cpp/models/ggml-base.en.bin", home)
    });
    let threads_env = env::var("THREADS").ok().or_else(|| env::var("WHISPER_THREADS").ok());
    let threads: usize = threads_env
        .as_deref()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10);
    let use_gpu = env::var("WHISPER_USE_GPU").unwrap_or_else(|_| "true".into());
    let concurrency: usize = 10;

    // Collect wav files in order
    let mut wavs: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(parts_dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("wav") {
            wavs.push(path);
        }
    }
    if wavs.is_empty() {
        return Err("no wav parts to transcribe".into());
    }
    wavs.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    let total = wavs.len();
    let _ = tx.send(WorkerMessage::DownloadLog(format!(
        "transcribing {} files with concurrency={}, threads={}",
        total, concurrency, threads
    )));
    let _ = tx.send(WorkerMessage::StageProgress {
        stage: Stage::Transcribe,
        ratio: 0.0,
        eta: None,
        note: Some(format!("0/{}", total)),
    });

    // Process in batches up to `concurrency`
    let mut idx = 0;
    let mut completed = 0usize;
    while idx < wavs.len() {
        if cancel.load(Ordering::SeqCst) {
            return Err("canceled".into());
        }

        let end = (idx + concurrency).min(wavs.len());
        let mut handles = Vec::new();

        for path in &wavs[idx..end] {
            let in_path = path.clone();
            let out_path = Path::new(parts_dir)
                .join(
                    Path::new(in_path.file_name().unwrap())
                        .with_extension("txt"),
                );
            let af = format!(
                "whisper=model={}:use_gpu={}:threads={}:destination={}:format=text:queue=3000ms",
                model_path,
                use_gpu,
                threads,
                out_path.display(),
            );

            let tx_clone = tx.clone();
            let cancel_clone = Arc::clone(cancel);
            let ffmpeg_bin_clone = ffmpeg_bin.clone();

            handles.push(thread::spawn(move || -> Result<(), String> {
                if cancel_clone.load(Ordering::SeqCst) {
                    return Err("canceled".into());
                }
                let _ = tx_clone.send(WorkerMessage::DownloadLog(format!(
                    "whisper: {} -> {}",
                    in_path.display(),
                    out_path.display()
                )));
                let status = Command::new(ffmpeg_bin_clone)
                    .args([
                        "-loglevel",
                        "error",
                        "-nostats",
                        "-i",
                        in_path.to_string_lossy().as_ref(),
                        "-af",
                        &af,
                        "-f",
                        "null",
                        "-",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .status()
                    .map_err(|e| e.to_string())?;
                if !status.success() {
                    let code = status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "terminated by signal".to_string());
                    return Err(format!(
                        "ffmpeg-whisper exited with {code} for {}",
                        in_path.display()
                    ));
                }
                Ok(())
            }));
        }

        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {
                    completed += 1;
                    let _ = tx.send(WorkerMessage::StageProgress {
                        stage: Stage::Transcribe,
                        ratio: (completed as f64) / (total as f64),
                        eta: None,
                        note: Some(format!("{}/{}", completed, total)),
                    });
                }
                Ok(Err(err)) => return Err(err),
                Err(_) => return Err("transcription thread panicked".into()),
            }
        }
        let _ = tx.send(WorkerMessage::DownloadLog(format!(
            "transcribed {}/{} parts",
            completed, total
        )));

        idx = end;
    }

    // Stitch into transcript.txt in order
    let transcript_path = Path::new("transcript.txt");
    let mut transcript = String::new();
    for path in &wavs {
        let txt = Path::new(parts_dir)
            .join(Path::new(path.file_name().unwrap()).with_extension("txt"));
        if txt.exists() {
            match fs::read_to_string(&txt) {
                Ok(mut s) => {
                    if !transcript.is_empty() && !transcript.ends_with('\n') {
                        transcript.push('\n');
                    }
                    transcript.push_str(&s);
                    if !transcript.ends_with('\n') {
                        transcript.push('\n');
                    }
                }
                Err(e) => return Err(format!("failed to read {}: {}", txt.display(), e)),
            }
        }
    }
    fs::write(transcript_path, transcript).map_err(|e| e.to_string())?;
    let _ = tx.send(WorkerMessage::StageProgress {
        stage: Stage::Transcribe,
        ratio: 1.0,
        eta: None,
        note: Some(format!("{}/{}", total, total)),
    });
    let _ = tx.send(WorkerMessage::DownloadLog(
        "transcript written to transcript.txt".to_string(),
    ));
    Ok(())
}
