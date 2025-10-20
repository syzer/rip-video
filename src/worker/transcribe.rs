use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::{Stage, WorkerMessage};
use crate::worker::audio_split::probe_duration_seconds;

pub(crate) fn transcribe_parts_10(
    parts_dir: &str,
    tx: &mpsc::Sender<WorkerMessage>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let ffmpeg_bin = std::env::var("FFMPEG_WHISPER_BIN")
        .unwrap_or_else(|_| format!("{}/.local/ffmpeg-whisper/bin/ffmpeg", home));
    let model_path = std::env::var("WHISPER_MODEL_PATH").unwrap_or_else(|_| {
        format!("{}/whisper.cpp/models/ggml-base.en.bin", home)
    });
    let threads_env = std::env::var("THREADS").ok().or_else(|| std::env::var("WHISPER_THREADS").ok());
    let threads: usize = threads_env
        .as_deref()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(10);
    let use_gpu = std::env::var("WHISPER_USE_GPU").unwrap_or_else(|_| "true".into());
    let concurrency: usize = 10;

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
        let done = txt.exists() && fs::metadata(&txt).map(|m| m.len() > 0).unwrap_or(false);
        skip.push(done);
    }
    let done_initial = skip.iter().filter(|b| **b).count();
    if done_initial == total && total > 0 {
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
    {
        if let Ok(mut vec) = progress_vec.lock() {
            for (i, sk) in skip.iter().enumerate() {
                vec[i] = if *sk { 1.0 } else { 0.0 };
            }
        }
    }
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

    let mut idx = 0;
    let mut _completed = 0usize;
    while idx < to_run.len() {
        if cancel.load(Ordering::SeqCst) {
            return Err("canceled".into());
        }
        let end = (idx + concurrency).min(to_run.len());
        let mut handles = Vec::new();
        for i_part in &to_run[idx..end] {
            let in_path = wavs[*i_part].clone();
            let out_path = Path::new(parts_dir)
                .join(Path::new(in_path.file_name().unwrap()).with_extension("txt"));
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
                                if let Some(val) = t.strip_prefix("out_time_us=")
                                    && let Ok(us) = val.parse::<u64>()
                                {
                                    let ratio = (us as f64 / 1_000_000.0) / part_duration;
                                    if let Ok(mut vec) = progress_vec_clone.lock() {
                                        vec[part_index] = ratio.clamp(0.0, 1.0);
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
                                if t == "progress=end" {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }

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

