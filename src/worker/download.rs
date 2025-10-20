use crate::Stage;
use crate::worker::audio_split::split_audio_to_parts;
use crate::worker::transcribe::transcribe_parts_10;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use crate::{sanitize_clipboard, WorkerMessage};

pub(crate) fn parse_ytdlp_progress(line: &str) -> Option<(f64, Option<String>)> {
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

pub(crate) fn resolve_ytdlp_output() -> String {
    std::env::var("YTDLP_OUTPUT").unwrap_or_else(|_| "audio.m4a".to_string())
}

pub fn download_worker(
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
    let _ = tx.send(WorkerMessage::StageProgress {
        stage: Stage::Download,
        ratio: 0.0,
        eta: None,
        note: None,
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
                    // Continue with splitting and transcription
                    match split_audio_to_parts(&output_file, "parts10", &tx, &cancel) {
                        Ok(()) => {
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
