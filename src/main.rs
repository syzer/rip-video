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
        Mutex,
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

mod banner;
mod keyboard;
mod ui;

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
enum Stage {
    Download,
    Split,
    Transcribe,
    Minutes,
}

enum WorkerMessage {
    // Legacy download progress
    Progress { ratio: f64, eta: Option<String> },
    // Stage-aware progress for multi-phase pipeline
    StageProgress { stage: Stage, ratio: f64, eta: Option<String>, note: Option<String> },
    Status(Result<(), String>),
    DownloadLog(String),
    MinutesChunk(String),
    MinutesDone(Result<(), String>),
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
    let mut minutes_progress: f64 = 0.0;
    let mut minutes_note: Option<String> = None;
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
                                Stage::Minutes => {
                                    minutes_progress = ratio.clamp(0.0, 1.0);
                                    minutes_note = note;
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
                                    // After full pipeline success: load transcript; spawn minutes stream via ollama if available
                                    if fs::metadata("transcript.txt").is_ok() {
                                        transcript_text = fs::read_to_string("transcript.txt").ok();
                                        if !tabs.iter().any(|t| t == "Transcription") {
                                            tabs.push("Transcription".to_string());
                                        }
                                        if !tabs.iter().any(|t| t == "Minutes") {
                                            tabs.push("Minutes".to_string());
                                        }
                                        let ollama_ok = Command::new("ollama")
                                            .arg("--version")
                                            .stdin(Stdio::null())
                                            .stdout(Stdio::null())
                                            .stderr(Stdio::null())
                                            .status()
                                            .ok()
                                            .map(|s| s.success())
                                            .unwrap_or(false);
                                        if ollama_ok {
                                            // Start minutes streaming thread
                                            minutes_text = Some(String::new());
                                            let (mtx, mrx) = mpsc::channel();
                                            let cancel_new = Arc::new(AtomicBool::new(false));
                                            let cancel_clone = Arc::clone(&cancel_new);
                                            // Progress timer 0->100% in 40s or until first output
                                            let mtx_timer = mtx.clone();
                                            thread::spawn(move || {
                                                let start = Instant::now();
                                                let total = Duration::from_secs(40);
                                                loop {
                                                    let elapsed = start.elapsed();
                                                    let ratio = (elapsed.as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0);
                                                    let _ = mtx_timer.send(WorkerMessage::StageProgress {
                                                        stage: Stage::Minutes,
                                                        ratio,
                                                        eta: None,
                                                        note: None,
                                                    });
                                                    if ratio >= 1.0 { break; }
                                                    thread::sleep(Duration::from_millis(500));
                                                }
                                            });
                                            thread::spawn(move || {
                                                // Stream: cat transcript.txt | ollama run minutes
                                                let path = Path::new("transcript.txt");
                                                let data = match fs::read(path) {
                                                    Ok(b) => b,
                                                    Err(e) => {
                                                        let _ = mtx.send(WorkerMessage::MinutesDone(Err(e.to_string())));
                                                        return;
                                                    }
                                                };
                                                let mut child = match Command::new("ollama")
                                                    .args(["run", "minutes"]) // requires installed minutes model
                                                    .stdin(Stdio::piped())
                                                    .stdout(Stdio::piped())
                                                    .stderr(Stdio::piped())
                                                    .spawn()
                                                {
                                                    Ok(c) => c,
                                                    Err(e) => {
                                                        let _ = mtx.send(WorkerMessage::MinutesDone(Err(e.to_string())));
                                                        return;
                                                    }
                                                };
                                                // Write transcript to stdin
                                                if let Some(mut stdin) = child.stdin.take() {
                                                    let _ = std::io::Write::write_all(&mut stdin, &data);
                                                }
                                                // Read stdout streaming
                                                if let Some(stdout) = child.stdout.take() {
                                                    let mut reader = BufReader::new(stdout);
                                                    let mut line = String::new();
                                                    let mut sent_first = false;
                                                    loop {
                                                        if cancel_clone.load(Ordering::SeqCst) {
                                                            let _ = child.kill();
                                                            let _ = mtx.send(WorkerMessage::MinutesDone(Err("canceled".into())));
                                                            return;
                                                        }
                                                        line.clear();
                                                        match reader.read_line(&mut line) {
                                                            Ok(0) => break,
                                                            Ok(_) => {
                                                                if !sent_first {
                                                                    sent_first = true;
                                                                    // set minutes prog to complete once first output appears
                                                                    let _ = mtx.send(WorkerMessage::StageProgress {
                                                                        stage: Stage::Minutes,
                                                                        ratio: 1.0,
                                                                        eta: None,
                                                                        note: Some("streaming".to_string()),
                                                                    });
                                                                }
                                                                let _ = mtx.send(WorkerMessage::MinutesChunk(line.clone()));
                                                            }
                                                            Err(_) => break,
                                                        }
                                                    }
                                                }
                                                // Wait and report status
                                                match child.wait() {
                                                    Ok(status) if status.success() => {
                                                        let _ = mtx.send(WorkerMessage::MinutesDone(Ok(())));
                                                    }
                                                    Ok(status) => {
                                                        let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "terminated by signal".to_string());
                                                        let _ = mtx.send(WorkerMessage::MinutesDone(Err(format!("ollama exit {code}"))));
                                                    }
                                                    Err(e) => {
                                                        let _ = mtx.send(WorkerMessage::MinutesDone(Err(e.to_string())));
                                                    }
                                                }
                                            });
                                            download_rx = Some(mrx);
                                            worker_cancel = Some(cancel_new);
                                            // Focus minutes tab to watch streaming
                                            if let Some(idx) = tabs.iter().position(|t| t == "Minutes") {
                                                selected_tab = idx;
                                            }
                                            // Reset scroll for minutes
                                            scroll_minutes = 0;
                                        } else {
                                            // Show requirement message if ollama missing
                                            minutes_text = Some(
                                                "Minutes generation requires ollama.\n\nInstall and run ollama, then create the minutes model:\n  ollama serve\n  ollama create minutes -f minutes.model\n\nTo generate minutes manually:\n  ollama run minutes < transcript.txt\n".to_string(),
                                            );
                                            if let Some(idx) = tabs.iter().position(|t| t == "Minutes") {
                                                selected_tab = idx;
                                            }
                                            scroll_minutes = 0;
                                        }
                                    }
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
                        WorkerMessage::MinutesChunk(chunk) => {
                            match minutes_text.as_mut() {
                                Some(s) => s.push_str(&chunk),
                                None => minutes_text = Some(chunk),
                            }
                        }
                        WorkerMessage::MinutesDone(res) => {
                            download_rx = None;
                            worker_cancel = None;
                            if res.is_ok() {
                                // Insert Q/A tab to the left of Minutes and select it
                                if let Some(min_idx) = tabs.iter().position(|t| t == "Minutes") {
                                    if !tabs.iter().any(|t| t == "Q/A") {
                                        tabs.insert(min_idx, "Q/A".to_string());
                                    }
                                    if let Some(qa_idx) = tabs.iter().position(|t| t == "Q/A") {
                                        selected_tab = qa_idx;
                                    }
                                } else if !tabs.iter().any(|t| t == "Q/A") {
                                    tabs.push("Q/A".to_string());
                                    if let Some(qa_idx) = tabs.iter().position(|t| t == "Q/A") {
                                        selected_tab = qa_idx;
                                    }
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
                &debug_lines,
                &tabs,
                selected_tab,
                transcript_text.as_deref(),
                minutes_text.as_deref(),
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
    command.args(args);
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
// chunk_text_to_width moved to ui module

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
    // Ensure the output directory exists (do not remove to preserve existing transcripts)
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

    // Probe durations and prepare weighted aggregation
    let mut durations: Vec<f64> = Vec::with_capacity(wavs.len());
    for path in &wavs {
        let d = probe_duration_seconds(path.to_string_lossy().as_ref())?;
        durations.push(d.max(0.001));
    }
    let total_duration: f64 = durations.iter().copied().sum::<f64>().max(0.001);
    let progress_vec = Arc::new(Mutex::new(vec![0.0_f64; wavs.len()]));

    let total = wavs.len();
    // Identify parts that already have TXT and can be skipped
    let mut skip: Vec<bool> = Vec::with_capacity(wavs.len());
    for path in &wavs {
        let txt = Path::new(parts_dir)
            .join(Path::new(path.file_name().unwrap()).with_extension("txt"));
        // consider only non-empty files as done
        let done = txt.exists() && fs::metadata(&txt).map(|m| m.len() > 0).unwrap_or(false);
        skip.push(done);
    }
    let done_initial = skip.iter().filter(|b| **b).count();
    if done_initial == total && total > 0 {
        // all parts already transcribed; emit 100% and return
        let _ = tx.send(WorkerMessage::StageProgress {
            stage: Stage::Transcribe,
            ratio: 1.0,
            eta: None,
            note: Some(format!("{}/{}", total, total)),
        });
        let _ = tx.send(WorkerMessage::DownloadLog(
            "all transcript parts present; skipping transcription".to_string(),
        ));
        return Ok(());
    }

    let to_run: Vec<usize> = (0..wavs.len()).filter(|i| !skip[*i]).collect();
    let _ = tx.send(WorkerMessage::DownloadLog(format!(
        "transcribing {} files with concurrency={}, threads={}",
        to_run.len(), concurrency, threads
    )));
    // Initialize progress vector, mark pre-existing parts complete
    {
        if let Ok(mut vec) = progress_vec.lock() {
            for (i, sk) in skip.iter().enumerate() {
                vec[i] = if *sk { 1.0 } else { 0.0 };
            }
        }
    }
    // Emit initial aggregate progress
    {
        if let Ok(vec) = progress_vec.lock() {
            let mut acc = 0.0;
            for (i, r) in vec.iter().enumerate() {
                acc += r.clamp(0.0, 1.0) * durations[i];
            }
            let agg = (acc / total_duration).clamp(0.0, 1.0);
            let _ = tx.send(WorkerMessage::StageProgress {
                stage: Stage::Transcribe,
                ratio: agg,
                eta: None,
                note: Some(format!("{}/{}", done_initial, total)),
            });
        }
    }

    // Process in batches up to `concurrency`
    // Build list of indices to process (skip existing) — already computed above as `to_run`
    let mut idx = 0;
    let mut _completed = 0usize; // counts only newly processed parts in this run
    while idx < to_run.len() {
        if cancel.load(Ordering::SeqCst) {
            return Err("canceled".into());
        }

        let end = (idx + concurrency).min(to_run.len());
        let mut handles = Vec::new();

        for i_part in &to_run[idx..end] {
            let in_path = wavs[*i_part].clone();
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
            let progress_vec_clone = Arc::clone(&progress_vec);
            let durations_clone = durations.clone();
            let total_duration_clone = total_duration;
            let part_index = *i_part;

            handles.push(thread::spawn(move || -> Result<(), String> {
                if cancel_clone.load(Ordering::SeqCst) {
                    return Err("canceled".into());
                }
                let _ = tx_clone.send(WorkerMessage::DownloadLog(format!(
                    "whisper: {} -> {}",
                    in_path.display(),
                    out_path.display()
                )));
                let mut child = Command::new(ffmpeg_bin_clone)
                    .args([
                        "-loglevel", "error",
                        "-nostats",
                        "-progress", "pipe:2",
                        "-stats_period", "0.5",
                        "-i", in_path.to_string_lossy().as_ref(),
                        "-af", &af,
                        "-f", "null", "-",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| e.to_string())?;

                // Progress reader
                if let Some(stderr) = child.stderr.take() {
                    let mut reader = BufReader::new(stderr);
                    let mut line = String::new();
                    let part_duration = durations_clone[part_index].max(0.001);
                    while !cancel_clone.load(Ordering::SeqCst) {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) => break,
                            Ok(_) => {
                                let t = line.trim();
                                if let Some(val) = t.strip_prefix("out_time_us=") {
                                    if let Ok(us) = val.parse::<u64>() {
                                        let ratio = (us as f64 / 1_000_000.0) / part_duration;
                                        if let Ok(mut vec) = progress_vec_clone.lock() {
                                            vec[part_index] = ratio.clamp(0.0, 1.0);
                                            // aggregate weighted ratio
                                            let mut acc = 0.0;
                                            for (i, r) in vec.iter().enumerate() {
                                                acc += r.clamp(0.0, 1.0) * durations_clone[i];
                                            }
                                            let agg = (acc / total_duration_clone).clamp(0.0, 1.0);
                                            let done = vec.iter().filter(|r| **r >= 0.999).count();
                                            let _ = tx_clone.send(WorkerMessage::StageProgress {
                                                stage: Stage::Transcribe,
                                                ratio: agg,
                                                eta: None,
                                                note: Some(format!("{}/{}", done, durations_clone.len())),
                                            });
                                        }
                                    }
                                }
                                if t == "progress=end" {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }

                // wait for child completion and handle cancel
                loop {
                    if cancel_clone.load(Ordering::SeqCst) {
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
                                    "ffmpeg-whisper exited with {code} for {}",
                                    in_path.display()
                                ));
                            }
                            // mark part complete
                            if let Ok(mut vec) = progress_vec_clone.lock() {
                                vec[part_index] = 1.0;
                                let mut acc = 0.0;
                                for (i, r) in vec.iter().enumerate() {
                                    acc += r.clamp(0.0, 1.0) * durations_clone[i];
                                }
                                let agg = (acc / total_duration_clone).clamp(0.0, 1.0);
                                let done = vec.iter().filter(|r| **r >= 0.999).count();
                                let _ = tx_clone.send(WorkerMessage::StageProgress {
                                    stage: Stage::Transcribe,
                                    ratio: agg,
                                    eta: None,
                                    note: Some(format!("{}/{}", done, durations_clone.len())),
                                });
                            }
                            break;
                        }
                        Ok(None) => {}
                        Err(err) => return Err(err.to_string()),
                    }
                    thread::sleep(Duration::from_millis(100));
                }

                Ok(())
            }));
        }

        for handle in handles {
            match handle.join() {
                Ok(Ok(())) => {
                    _completed += 1;
                    // ratio updates already emitted during progress; still emit a final step tick
                    if let Ok(vec) = progress_vec.lock() {
                        let mut acc = 0.0;
                        for (i, r) in vec.iter().enumerate() {
                            acc += r.clamp(0.0, 1.0) * durations[i];
                        }
                        let agg = (acc / total_duration).clamp(0.0, 1.0);
                        let done = vec.iter().filter(|r| **r >= 0.999).count();
                        let _ = tx.send(WorkerMessage::StageProgress {
                            stage: Stage::Transcribe,
                            ratio: agg,
                            eta: None,
                            note: Some(format!("{}/{}", done, total)),
                        });
                    }
                }
                Ok(Err(err)) => return Err(err),
                Err(_) => return Err("transcription thread panicked".into()),
            }
        }
        // Log status including pre-existing done parts
        if let Ok(vec) = progress_vec.lock() {
            let done = vec.iter().filter(|r| **r >= 0.999).count();
            let _ = tx.send(WorkerMessage::DownloadLog(format!(
                "transcribed {}/{} parts (including pre-existing)",
                done, total
            )));
        }

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
                Ok(s) => {
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
