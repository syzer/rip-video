use std::fs;
// removed unused BufRead/BufReader imports
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use crate::{Stage, WorkerMessage};

pub(crate) fn split_audio_to_parts(
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

pub(crate) fn probe_duration_seconds(input_path: &str) -> Result<f64, String> {
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
        .args(args)
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
