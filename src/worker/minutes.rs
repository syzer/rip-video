use crate::{Stage, WorkerMessage};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

pub fn start_minutes_worker(
) -> Result<(mpsc::Receiver<WorkerMessage>, Arc<AtomicBool>), String> {
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
            "Minutes generation requires ollama.\n\nInstall and run ollama, then create the minutes model:\n  ollama serve\n  ollama create minutes -f minutes.model\n\nTo generate minutes manually:\n  ollama run minutes < transcript.txt\n"
                .to_string(),
        );
    }

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
            if ratio >= 1.0 {
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
    });

    // Spawn ollama minutes streaming
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
                let code = status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string());
                let _ = mtx.send(WorkerMessage::MinutesDone(Err(format!(
                    "ollama exit {code}"
                ))));
            }
            Err(e) => {
                let _ = mtx.send(WorkerMessage::MinutesDone(Err(e.to_string())));
            }
        }
    });

    Ok((mrx, cancel_new))
}

