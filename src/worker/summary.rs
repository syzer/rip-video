use crate::{Stage, WorkerMessage};
use std::fs;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

pub fn start_summary_worker() -> Result<(mpsc::Receiver<WorkerMessage>, Arc<AtomicBool>), String> {
    // Check if ollama is available
    let ollama_ok = Command::new("ollama")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()
        .map(|s| s.success())
        .unwrap_or(false);

    if !ollama_ok {
        return Err(
            "Summary generation requires ollama.\n\nInstall and run ollama:\n  ollama serve\n  ollama pull deepseek-r1:14b\nThen rerun the app.".to_string(),
        );
    }

    let (stx, srx) = mpsc::channel();
    let cancel_new = Arc::new(AtomicBool::new(false));
    let cancel_clone = Arc::clone(&cancel_new);

    // Start summary warm-up progress timer similar to Minutes (40s)
    let stx_timer = stx.clone();
    thread::spawn(move || {
        let start = Instant::now();
        let total = Duration::from_secs(40);
        loop {
            let ratio = (start.elapsed().as_secs_f64() / total.as_secs_f64()).clamp(0.0, 1.0);
            let _ = stx_timer.send(WorkerMessage::StageProgress {
                stage: Stage::Summary,
                ratio,
                eta: None,
                note: None,
            });
            if ratio >= 1.0 {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
    });

    // Spawn ollama summary streaming
    thread::spawn(move || {
        let transcript = match fs::read_to_string("transcript.txt") {
            Ok(s) => s,
            Err(e) => {
                let _ = stx.send(WorkerMessage::SummaryDone(Err(e.to_string())));
                return;
            }
        };
        let prompt = format!(
            "Make a summary of this transcript:\n{}",
            transcript
        );
        let mut child = match Command::new("ollama")
            .args(["run", "deepseek-r1:14b", &prompt])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = stx.send(WorkerMessage::SummaryDone(Err(e.to_string())));
                return;
            }
        };
        if let Some(stdout) = child.stdout.take() {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut first = true;
            loop {
                if cancel_clone.load(Ordering::SeqCst) {
                    let _ = child.kill();
                    let _ = stx.send(WorkerMessage::SummaryDone(Err("canceled".into())));
                    return;
                }
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if first {
                            first = false;
                            let _ = stx.send(WorkerMessage::StageProgress {
                                stage: Stage::Summary,
                                ratio: 1.0,
                                eta: None,
                                note: Some("streaming".to_string()),
                            });
                        }
                        let _ = stx.send(WorkerMessage::SummaryChunk(line.clone()));
                    }
                    Err(_) => break,
                }
            }
        }
        match child.wait() {
            Ok(status) if status.success() => {
                let _ = stx.send(WorkerMessage::SummaryDone(Ok(())));
            }
            Ok(status) => {
                let code = status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string());
                let _ = stx.send(WorkerMessage::SummaryDone(Err(format!(
                    "ollama exit {code}"
                ))));
            }
            Err(e) => {
                let _ = stx.send(WorkerMessage::SummaryDone(Err(e.to_string())));
            }
        }
    });

    Ok((srx, cancel_new))
}

