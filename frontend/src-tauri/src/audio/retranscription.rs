// Retranscription module - allows re-processing stored audio with different settings

use crate::audio::decoder::decode_audio_file;
use crate::audio::vad::get_speech_chunks_with_progress;
use super::common::{create_transcript_segments, create_transcript_segments_with_speaker, split_segment_at_silence, write_transcripts_json};
use super::constants::AUDIO_EXTENSIONS;
use crate::config::{DEFAULT_WHISPER_MODEL, DEFAULT_PARAKEET_MODEL};
use crate::parakeet_engine::ParakeetEngine;
use crate::state::AppState;
use crate::whisper_engine::WhisperEngine;
use anyhow::{anyhow, Result};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Global flag to track if retranscription is in progress
static RETRANSCRIPTION_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

/// Global flag to signal cancellation
static RETRANSCRIPTION_CANCELLED: AtomicBool = AtomicBool::new(false);

/// RAII guard for RETRANSCRIPTION_IN_PROGRESS flag
/// Ensures flag is cleared even if retranscription panics or returns early
struct RetranscriptionGuard;

impl RetranscriptionGuard {
    /// Create guard and set flag atomically
    fn acquire() -> Result<Self, String> {
        if RETRANSCRIPTION_IN_PROGRESS
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("Retranscription already in progress".to_string());
        }
        Ok(RetranscriptionGuard)
    }
}

impl Drop for RetranscriptionGuard {
    fn drop(&mut self) {
        RETRANSCRIPTION_IN_PROGRESS.store(false, Ordering::SeqCst);
    }
}

/// VAD redemption time in milliseconds - bridges natural pauses in speech
/// Batch processing needs longer redemption (2000ms) than live pipeline (400ms)
/// because the entire file is processed at once by VAD, and 400ms fragments
/// speech at every natural sentence/topic pause (500ms-2s)
const VAD_REDEMPTION_TIME_MS: u32 = 2000;

/// Progress update emitted during retranscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetranscriptionProgress {
    pub meeting_id: String,
    pub stage: String, // "decoding", "transcribing", "saving"
    pub progress_percentage: u32,
    pub message: String,
}

/// Result of retranscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetranscriptionResult {
    pub meeting_id: String,
    pub segments_count: usize,
    pub duration_seconds: f64,
    pub language: Option<String>,
}

/// Error during retranscription
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetranscriptionError {
    pub meeting_id: String,
    pub error: String,
}

/// Result of a standalone speaker-identification pass (see `run_speaker_identification`)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerIdentificationResult {
    pub meeting_id: String,
    pub speakers_found: usize,
    pub segments_updated: usize,
}

/// Check if retranscription is currently in progress
pub fn is_retranscription_in_progress() -> bool {
    RETRANSCRIPTION_IN_PROGRESS.load(Ordering::SeqCst)
}

/// Cancel ongoing retranscription
pub fn cancel_retranscription() {
    RETRANSCRIPTION_CANCELLED.store(true, Ordering::SeqCst);
}

/// Start retranscription of a meeting's audio
pub async fn start_retranscription<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    language: Option<String>,
    model: Option<String>,
    provider: Option<String>,
) -> Result<RetranscriptionResult> {
    // Acquire guard - ensures flag is cleared even on panic/early return
    let _guard = RetranscriptionGuard::acquire().map_err(|e| anyhow!(e))?;

    // Reset cancellation flag
    RETRANSCRIPTION_CANCELLED.store(false, Ordering::SeqCst);

    let use_parakeet = provider.as_deref() == Some("parakeet");
    let result = run_retranscription(app.clone(), meeting_id.clone(), meeting_folder_path, language, model, provider).await;

    // Unload the engine after the batch job (success, failure, or cancellation)
    super::common::unload_engine_after_batch(use_parakeet).await;

    // Guard will automatically clear flag on drop
    // No need for manual: RETRANSCRIPTION_IN_PROGRESS.store(false, Ordering::SeqCst);

    match &result {
        Ok(res) => {
            let _ = app.emit(
                "retranscription-complete",
                serde_json::json!({
                    "meeting_id": res.meeting_id,
                    "segments_count": res.segments_count,
                    "duration_seconds": res.duration_seconds,
                    "language": res.language
                }),
            );
        }
        Err(e) => {
            let _ = app.emit(
                "retranscription-error",
                RetranscriptionError {
                    meeting_id: meeting_id.clone(),
                    error: e.to_string(),
                },
            );
        }
    }

    result
}

/// Find audio file in meeting folder
/// Tries common names first, then scans for any file with an audio extension
fn find_audio_file(folder: &Path) -> Result<PathBuf> {
    let candidates = [
        "audio.mp4", "audio.m4a", "audio.wav", "audio.mp3",
        "audio.flac", "audio.ogg", "recording.mp4",
        "audio.mkv", "audio.webm", "audio.wma",
    ];

    for name in candidates {
        let path = folder.join(name);
        if path.exists() {
            return Ok(path);
        }
    }

    // Fallback: scan folder for any file with an audio extension
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                let ext = ext.to_string_lossy().to_lowercase();
                if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
                    return Ok(path);
                }
            }
        }
    }

    Err(anyhow!("No audio file found in: {}", folder.display()))
}

/// Find a single raw track file (e.g. "mic" or "system") by base name, trying each
/// supported extension in turn. Returns `None` if no matching file exists.
fn find_track_file(folder: &Path, base_name: &str) -> Option<PathBuf> {
    let extensions = ["mp4", "m4a", "wav", "mp3", "flac", "ogg", "mkv", "webm", "wma"];
    extensions
        .iter()
        .map(|ext| folder.join(format!("{}.{}", base_name, ext)))
        .find(|path| path.exists())
}

/// Find a pair of raw mic/system audio files in a meeting folder, if both exist.
/// These are only present for meetings recorded in "record only" mode (see
/// recording_saver.rs / pipeline.rs) - live-transcribed meetings never write a
/// `mic.*` track, since attribution already happens in real time in that mode.
///
/// Returns `None` (not an error) when either file is missing, so callers fall back
/// to the normal single-file (mixed audio, no speaker attribution) path.
fn find_dual_audio_files(folder: &Path) -> Option<(PathBuf, PathBuf)> {
    match (find_track_file(folder, "mic"), find_track_file(folder, "system")) {
        (Some(mic), Some(system)) => Some((mic, system)),
        _ => None,
    }
}

/// Find a standalone raw `system.*` track without requiring a matching `mic.*`.
/// Live-transcribed meetings persist exactly this (see recording_saver.rs) so a
/// later on-demand diarization pass can identify individual speakers within
/// "system" without re-transcribing - see `run_speaker_identification` below.
fn find_system_audio_file(folder: &Path) -> Option<PathBuf> {
    find_track_file(folder, "system")
}

/// Map a 0-100 "local" progress percentage (this file's own decode/VAD/transcribe
/// phases) into the caller's overall progress bar range.
fn map_progress(local_pct: u32, range_start: u32, range_span: f32) -> u32 {
    range_start + ((local_pct.min(100) as f32 / 100.0) * range_span) as u32
}

/// Decode, VAD, and transcribe a single audio file. Shared by both the single-file
/// (mixed audio, no speaker attribution) and dual-file (separate mic/system tracks,
/// speaker attribution preserved) retranscription paths in `run_retranscription`.
///
/// `progress_range` is the (start_pct, end_pct) window this file's phases should
/// report progress within, so two files can share one 0-100% progress bar.
///
/// Returns `(transcripts, duration_seconds)`. An empty `transcripts` list is NOT an
/// error - it's the normal outcome for a track with no speech at all (e.g. the mic
/// track of a meeting where only other participants talked).
#[allow(clippy::too_many_arguments)]
async fn transcribe_audio_file<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    audio_path: &Path,
    language: Option<String>,
    use_parakeet: bool,
    whisper_engine: Option<&Arc<WhisperEngine>>,
    parakeet_engine: Option<&Arc<ParakeetEngine>>,
    label: &str,
    progress_range: (u32, u32),
) -> Result<(Vec<(String, f64, f64)>, f64)> {
    let (range_start, range_end) = progress_range;
    let range_span = (range_end - range_start) as f32;

    emit_progress(app, meeting_id, "decoding", map_progress(0, range_start, range_span),
        &format!("Decoding {} audio...", label));
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    // Decode the audio file (CPU-intensive, run in blocking task)
    let path_for_decode = audio_path.to_path_buf();
    let decoded = tokio::task::spawn_blocking(move || decode_audio_file(&path_for_decode))
        .await
        .map_err(|e| anyhow!("Decode task panicked: {}", e))??;
    let duration_seconds = decoded.duration_seconds;

    info!(
        "Decoded {} audio: {:.2}s, {}Hz, {} channels",
        label, duration_seconds, decoded.sample_rate, decoded.channels
    );

    emit_progress(app, meeting_id, "decoding", map_progress(15, range_start, range_span),
        &format!("Converting {} audio format...", label));
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    // Convert to 16kHz mono format (CPU-intensive, run in blocking task)
    let audio_samples = tokio::task::spawn_blocking(move || decoded.to_whisper_format())
        .await
        .map_err(|e| anyhow!("Resample task panicked: {}", e))?;
    info!("Converted {} audio to 16kHz mono format: {} samples", label, audio_samples.len());

    emit_progress(app, meeting_id, "vad", map_progress(20, range_start, range_span),
        &format!("Detecting speech in {} audio...", label));
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    // Use VAD to find natural speech boundaries (same approach as live transcription)
    // IMPORTANT: Run VAD in a blocking task to avoid blocking the async runtime
    let app_for_vad = app.clone();
    let meeting_id_for_vad = meeting_id.to_string();
    let label_for_vad = label.to_string();

    let speech_segments = tokio::task::spawn_blocking(move || {
        get_speech_chunks_with_progress(
            &audio_samples,
            VAD_REDEMPTION_TIME_MS,
            |vad_progress, segments_found| {
                let overall_progress = map_progress(
                    20 + (vad_progress as f32 * 0.05) as u32,
                    range_start,
                    range_span,
                );
                emit_progress(
                    &app_for_vad,
                    &meeting_id_for_vad,
                    "vad",
                    overall_progress,
                    &format!("Detecting speech in {} audio... {}% ({} found)", label_for_vad, vad_progress, segments_found),
                );
                !RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst)
            },
        )
    })
    .await
    .map_err(|e| anyhow!("VAD task panicked: {}", e))?
    .map_err(|e| anyhow!("VAD processing failed: {}", e))?;

    let total_segments = speech_segments.len();
    info!("VAD detected {} speech segments in {} audio (redemption_time={}ms)", total_segments, label, VAD_REDEMPTION_TIME_MS);

    if total_segments == 0 {
        // Not an error here: perfectly normal for one side of a dual-track meeting to
        // have no speech at all. The caller decides whether the overall result (across
        // both tracks, for dual mode) counts as "nothing found".
        warn!("No speech detected in {} audio", label);
        return Ok((Vec::new(), duration_seconds));
    }

    // Diagnostic: log segment duration distribution
    let durations_ms: Vec<f64> = speech_segments.iter()
        .map(|s| s.end_timestamp_ms - s.start_timestamp_ms)
        .collect();
    let total_speech_ms: f64 = durations_ms.iter().sum();
    let avg_duration = total_speech_ms / durations_ms.len() as f64;
    info!(
        "{} VAD segment stats: avg={:.0}ms, total_speech={:.1}s/{:.1}s ({:.0}%)",
        label, avg_duration, total_speech_ms / 1000.0, duration_seconds,
        (total_speech_ms / 1000.0 / duration_seconds) * 100.0
    );

    emit_progress(app, meeting_id, "transcribing", map_progress(25, range_start, range_span),
        &format!("Loading transcription engine for {} audio...", label));

    // Split very long segments at silence boundaries for better transcription quality.
    // Hard cuts at arbitrary sample positions lose words at boundaries. Instead, scan
    // for the lowest-energy window near the target split point and cut there.
    const MAX_SEGMENT_SAMPLES: usize = 25 * 16000; // 25 seconds at 16kHz

    let mut processable_segments: Vec<crate::audio::vad::SpeechSegment> = Vec::new();
    for segment in &speech_segments {
        if segment.samples.len() > MAX_SEGMENT_SAMPLES {
            debug!(
                "Splitting large {} segment ({:.0}ms, {} samples) at silence boundaries",
                label, segment.end_timestamp_ms - segment.start_timestamp_ms, segment.samples.len()
            );
            let sub_segments = split_segment_at_silence(segment, MAX_SEGMENT_SAMPLES);
            debug!("Split into {} sub-segments", sub_segments.len());
            processable_segments.extend(sub_segments);
        } else {
            processable_segments.push(segment.clone());
        }
    }

    let processable_count = processable_segments.len();
    info!("Processing {} {} segments (after splitting)", processable_count, label);

    // Process each speech segment with progress updates
    let mut all_transcripts: Vec<(String, f64, f64)> = Vec::new(); // (text, start_ms, end_ms)

    for (i, segment) in processable_segments.iter().enumerate() {
        if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
            return Err(anyhow!("Retranscription cancelled"));
        }

        // Local transcribing phase spans 25-100 within this file's own progress window
        let local_progress = 25 + ((i as f32 / processable_count as f32) * 75.0) as u32;
        let segment_duration_sec = (segment.end_timestamp_ms - segment.start_timestamp_ms) / 1000.0;
        emit_progress(
            app,
            meeting_id,
            "transcribing",
            map_progress(local_progress, range_start, range_span),
            &format!(
                "Transcribing {} segment {} of {} ({:.1}s)...",
                label, i + 1, processable_count, segment_duration_sec
            ),
        );

        // Skip very short segments (< 100ms of audio = 1600 samples at 16kHz)
        if segment.samples.len() < 1600 {
            debug!("Skipping short {} segment {} with {} samples", label, i, segment.samples.len());
            continue;
        }

        // Transcribe this segment
        let (text, conf) = if use_parakeet {
            let engine = parakeet_engine.ok_or_else(|| anyhow!("Parakeet engine not initialized"))?;
            let text = engine
                .transcribe_audio(segment.samples.clone())
                .await
                .map_err(|e| anyhow!("Parakeet transcription failed on {} segment {}: {}", label, i, e))?;
            (text, 0.9f32)
        } else {
            let engine = whisper_engine.ok_or_else(|| anyhow!("Whisper engine not initialized"))?;
            let (text, conf, _) = engine
                .transcribe_audio_with_confidence(segment.samples.clone(), language.clone())
                .await
                .map_err(|e| anyhow!("Whisper transcription failed on {} segment {}: {}", label, i, e))?;
            (text, conf)
        };

        // Skip empty transcripts
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            debug!(
                "{} segment {}/{}: {:.1}s, conf={:.2}, text='{}'",
                label, i + 1, processable_count, segment_duration_sec, conf,
                if trimmed.len() > 80 { let mut end = 80; while !trimmed.is_char_boundary(end) { end -= 1; } &trimmed[..end] } else { trimmed }
            );
            all_transcripts.push((text, segment.start_timestamp_ms, segment.end_timestamp_ms));
        } else {
            debug!("{} segment {}/{}: {:.1}s — empty transcription", label, i + 1, processable_count, segment_duration_sec);
        }
    }

    info!("{} transcription complete: {} segments transcribed out of {}", label, all_transcripts.len(), processable_count);

    Ok((all_transcripts, duration_seconds))
}

/// Internal function to run retranscription
async fn run_retranscription<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    language: Option<String>,
    model: Option<String>,
    provider: Option<String>,
) -> Result<RetranscriptionResult> {
    let folder_path = PathBuf::from(&meeting_folder_path);

    // Determine which provider to use (default to whisper)
    let use_parakeet = provider.as_deref() == Some("parakeet");

    info!(
        "Starting retranscription for meeting {} with language {:?}, model {:?}, provider {:?}",
        meeting_id, language, model, provider
    );

    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    // Initialize the appropriate engine once (shared across one or two files)
    emit_progress(&app, &meeting_id, "loading", 2, "Loading transcription engine...");
    let whisper_engine = if !use_parakeet {
        Some(get_or_init_whisper(&app, model.as_deref()).await?)
    } else {
        None
    };
    let parakeet_engine = if use_parakeet {
        Some(get_or_init_parakeet(&app, model.as_deref()).await?)
    } else {
        None
    };

    // "Record only" meetings keep separate raw mic/system tracks (see
    // recording_saver.rs) so retranscription can still attribute segments to a
    // speaker; regular live-transcribed or imported meetings only ever have the
    // single mixed audio.mp4, so fall back to that with no speaker attribution.
    let mut diarization_system_path: Option<PathBuf> = None;

    let (mut segments, duration_seconds, audio_filename) = if let Some((mic_path, system_path)) =
        find_dual_audio_files(&folder_path)
    {
        info!("Found separate mic/system tracks - transcribing with speaker attribution");

        let (mic_transcripts, mic_duration) = transcribe_audio_file(
            &app, &meeting_id, &mic_path, language.clone(), use_parakeet,
            whisper_engine.as_ref(), parakeet_engine.as_ref(), "mic", (5, 40),
        ).await?;

        let (system_transcripts, system_duration) = transcribe_audio_file(
            &app, &meeting_id, &system_path, language.clone(), use_parakeet,
            whisper_engine.as_ref(), parakeet_engine.as_ref(), "system", (40, 75),
        ).await?;

        if mic_transcripts.is_empty() && system_transcripts.is_empty() {
            warn!("No speech detected in either mic or system audio");
            return Err(anyhow!("No speech detected in audio file"));
        }

        let mut segments = create_transcript_segments_with_speaker(&mic_transcripts, Some("mic"));
        segments.extend(create_transcript_segments_with_speaker(&system_transcripts, Some("system")));
        // Merge mic + system chronologically, matching how live recording interleaves them
        segments.sort_by(|a, b| {
            a.audio_start_time.partial_cmp(&b.audio_start_time).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Diarization needs the system track's own decoded audio again (to slice
        // per-segment embeddings) - remembered for the best-effort step below,
        // which runs after this branch so it can populate speaker_label directly
        // on `segments` before they're ever inserted into the DB.
        diarization_system_path = Some(system_path);

        (segments, mic_duration.max(system_duration), "audio.mp4".to_string())
    } else {
        let audio_path = find_audio_file(&folder_path)?;
        let (transcripts, duration_seconds) = transcribe_audio_file(
            &app, &meeting_id, &audio_path, language.clone(), use_parakeet,
            whisper_engine.as_ref(), parakeet_engine.as_ref(), "audio", (5, 85),
        ).await?;

        if transcripts.is_empty() {
            warn!("No speech detected in audio");
            return Err(anyhow!("No speech detected in audio file"));
        }

        let segments = create_transcript_segments(&transcripts);
        let audio_filename = audio_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio.mp4")
            .to_string();

        (segments, duration_seconds, audio_filename)
    };

    // Check for cancellation
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    // Best-effort speaker diarization: identify distinct speakers within "system"
    // and set speaker_label directly on the segments before they're saved. Only
    // for "record only" meetings (dual mic/system tracks); never fails the
    // transcription itself - any problem here just leaves labels unset, same as
    // "Others" looks today.
    if let Some(system_path) = &diarization_system_path {
        emit_progress(&app, &meeting_id, "diarizing", 80, "Identifying speakers...");
        match run_diarization(&app, &meeting_id, &mut segments, system_path).await {
            Ok(count) => info!("Diarization found {} distinct speaker(s)", count),
            Err(e) => warn!("Diarization skipped/failed (transcription is unaffected): {}", e),
        }
    }

    // Check for cancellation
    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    emit_progress(&app, &meeting_id, "saving", 85, "Saving transcripts...");

    // Save to database
    let app_state = app
        .try_state::<AppState>()
        .ok_or_else(|| anyhow!("App state not available"))?;

    // Wrap delete+insert+update in a transaction to prevent data loss
    let pool = app_state.db_manager.pool();
    let mut conn = pool.acquire().await.map_err(|e| anyhow!("DB error: {}", e))?;
    let mut tx = sqlx::Connection::begin(&mut *conn)
        .await
        .map_err(|e| anyhow!("Failed to start transaction: {}", e))?;

    sqlx::query("DELETE FROM transcripts WHERE meeting_id = ?")
        .bind(&meeting_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| anyhow!("Failed to delete existing transcripts: {}", e))?;

    for segment in &segments {
        sqlx::query(
            "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration, speaker, speaker_label)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&segment.id)
        .bind(&meeting_id)
        .bind(&segment.text)
        .bind(&segment.timestamp)
        .bind(segment.audio_start_time)
        .bind(segment.audio_end_time)
        .bind(segment.duration)
        .bind(&segment.speaker)
        .bind(&segment.speaker_label)
        .execute(&mut *tx)
        .await
        .map_err(|e| anyhow!("Failed to insert transcript: {}", e))?;
    }

    // Mark the meeting as transcribed - clears the "pending" state a "record only"
    // meeting was saved with (see useRecordingStop.ts), so meeting-details stops
    // offering the "Transcribe" action and shows the transcript instead.
    sqlx::query("UPDATE meetings SET transcription_status = 'completed', updated_at = ? WHERE id = ?")
        .bind(chrono::Utc::now())
        .bind(&meeting_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| anyhow!("Failed to update meeting transcription_status: {}", e))?;

    tx.commit().await
        .map_err(|e| anyhow!("Failed to commit transaction: {}", e))?;

    info!(
        "Updated {} transcripts for meeting {} in transaction",
        segments.len(),
        meeting_id
    );

    // Write updated transcripts.json and metadata.json to the meeting folder
    emit_progress(&app, &meeting_id, "saving", 95, "Writing transcript files...");

    if let Err(e) = write_transcripts_json(&folder_path, &segments) {
        warn!("Failed to write transcripts.json: {}", e);
    }

    if let Err(e) = write_retranscription_metadata(
        &folder_path,
        &meeting_id,
        duration_seconds,
        &audio_filename,
    ) {
        warn!("Failed to update metadata.json: {}", e);
    }

    emit_progress(&app, &meeting_id, "complete", 100, "Retranscription complete");

    Ok(RetranscriptionResult {
        meeting_id,
        segments_count: segments.len(),
        duration_seconds,
        language,
    })
}

/// Run a standalone diarization pass against a meeting's EXISTING transcript rows -
/// no re-transcription involved. Loads the meeting's transcripts straight from the
/// database, finds its persisted raw `system.*` track (recording_saver.rs persists
/// this for live-transcribed meetings too now, not just "record only" ones), runs
/// the same `run_diarization` full retranscription uses, and writes any resulting
/// `speaker_label`s back onto the existing rows in place.
async fn run_speaker_identification<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
) -> Result<SpeakerIdentificationResult> {
    let folder_path = PathBuf::from(&meeting_folder_path);

    info!("Starting speaker identification for meeting {}", meeting_id);
    emit_progress(app, &meeting_id, "loading", 5, "Loading transcript...");

    let app_state = app
        .try_state::<AppState>()
        .ok_or_else(|| anyhow!("App state not available"))?;
    let pool = app_state.db_manager.pool();

    let (transcripts, _total) =
        crate::database::repositories::meeting::MeetingsRepository::get_meeting_transcripts_paginated(
            pool,
            &meeting_id,
            i64::MAX,
            0,
        )
        .await
        .map_err(|e| anyhow!("Failed to load transcripts: {}", e))?;

    if transcripts.is_empty() {
        return Err(anyhow!("This meeting has no transcript yet"));
    }

    let mut segments: Vec<crate::api::TranscriptSegment> = transcripts
        .into_iter()
        .map(|t| crate::api::TranscriptSegment {
            id: t.id,
            text: t.transcript,
            timestamp: t.timestamp,
            audio_start_time: t.audio_start_time,
            audio_end_time: t.audio_end_time,
            duration: t.duration,
            speaker: t.speaker,
            speaker_label: t.speaker_label,
        })
        .collect();

    if !segments.iter().any(|s| s.speaker.as_deref() == Some("system")) {
        return Err(anyhow!(
            "No system-audio segments found for this meeting - nothing to identify speakers in"
        ));
    }

    let system_path = find_system_audio_file(&folder_path).ok_or_else(|| {
        anyhow!(
            "No separate system-audio track found for this meeting. Speaker identification is \
             only available for meetings recorded after this feature was added."
        )
    })?;

    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    let speakers_found = run_diarization(app, &meeting_id, &mut segments, &system_path).await?;

    emit_progress(app, &meeting_id, "saving", 97, "Saving speaker labels...");

    // Persist: update every "system" row's speaker_label (not just the ones that
    // changed) - this also clears stale labels on a re-run, which matters once
    // someone recalibrates the clustering threshold and re-triggers this.
    let mut conn = pool.acquire().await.map_err(|e| anyhow!("DB error: {}", e))?;
    let mut tx = sqlx::Connection::begin(&mut *conn)
        .await
        .map_err(|e| anyhow!("Failed to start transaction: {}", e))?;

    let mut segments_updated = 0usize;
    for segment in &segments {
        if segment.speaker.as_deref() != Some("system") {
            continue;
        }
        sqlx::query("UPDATE transcripts SET speaker_label = ? WHERE id = ?")
            .bind(&segment.speaker_label)
            .bind(&segment.id)
            .execute(&mut *tx)
            .await
            .map_err(|e| anyhow!("Failed to update speaker_label: {}", e))?;
        segments_updated += 1;
    }

    tx.commit()
        .await
        .map_err(|e| anyhow!("Failed to commit transaction: {}", e))?;

    if let Err(e) = write_transcripts_json(&folder_path, &segments) {
        warn!("Failed to write transcripts.json after speaker identification: {}", e);
    }

    emit_progress(app, &meeting_id, "complete", 100, "Speaker identification complete");

    info!(
        "Speaker identification complete for meeting {}: {} distinct speakers, {} segments updated",
        meeting_id, speakers_found, segments_updated
    );

    Ok(SpeakerIdentificationResult {
        meeting_id,
        speakers_found,
        segments_updated,
    })
}

/// Public entry point for the "Identify speakers" action - acquires the shared
/// retranscription/diarization job guard and emits completion/error events,
/// mirroring `start_retranscription`.
pub async fn start_speaker_identification<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
) -> Result<SpeakerIdentificationResult> {
    let _guard = RetranscriptionGuard::acquire().map_err(|e| anyhow!(e))?;
    RETRANSCRIPTION_CANCELLED.store(false, Ordering::SeqCst);

    let result = run_speaker_identification(&app, meeting_id.clone(), meeting_folder_path).await;

    match &result {
        Ok(res) => {
            let _ = app.emit("diarization-complete", res);
        }
        Err(e) => {
            let _ = app.emit(
                "diarization-error",
                RetranscriptionError {
                    meeting_id: meeting_id.clone(),
                    error: e.to_string(),
                },
            );
        }
    }

    result
}

/// Best-effort offline speaker diarization over the "system" segments of a meeting.
/// Decodes `system_path` once, slices out each "system" segment's own audio by its
/// already-computed timestamps (no second VAD pass - reuses whatever timestamps the
/// caller already has, whether from a just-finished offline transcription or from a
/// live session read back out of the database), extracts a speaker embedding per
/// segment, clusters them, and sets `speaker_label` directly on the matching entries
/// in `segments` ("Speaker 1", "Speaker 2", ... ordered by first chronological
/// appearance). Only writes labels when clustering actually found 2+ distinct
/// speakers - a single detected voice stays plain "Others", since a redundant
/// "Speaker 1" label wouldn't add information.
///
/// Returns the number of distinct speakers found (0 when clustering didn't split).
/// Returns `Err` (never `panic`s) on any problem - missing/undownloaded model,
/// decode failure, inference failure. Callers should log and continue; this must
/// never fail the transcription it's attached to.
async fn run_diarization<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    segments: &mut [crate::api::TranscriptSegment],
    system_path: &Path,
) -> Result<usize> {
    emit_progress(app, meeting_id, "diarizing", 82, "Loading speaker identification model...");

    crate::diarization_engine::commands::diarization_init()
        .await
        .map_err(|e| anyhow!(e))?;

    let engine = {
        let guard = crate::diarization_engine::commands::DIARIZATION_ENGINE
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.as_ref().cloned()
    }
    .ok_or_else(|| anyhow!("Diarization engine not initialized"))?;

    if !engine.is_model_ready().await {
        return Err(anyhow!(
            "Speaker embedding model not downloaded (Settings > Speaker Identification)"
        ));
    }

    if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
        return Err(anyhow!("Retranscription cancelled"));
    }

    // Decode the system track once (independently of the earlier transcription
    // pass - that decoded buffer wasn't kept around).
    emit_progress(app, meeting_id, "diarizing", 85, "Decoding system audio...");
    let path_for_decode = system_path.to_path_buf();
    let decoded = tokio::task::spawn_blocking(move || decode_audio_file(&path_for_decode))
        .await
        .map_err(|e| anyhow!("Decode task panicked: {}", e))??;
    let samples = decoded.to_whisper_format(); // 16kHz mono, matches segment timestamps

    // Collect one embedding per "system" segment, remembering which segment (by
    // index) each embedding belongs to.
    emit_progress(app, meeting_id, "diarizing", 88, "Extracting speaker embeddings...");
    let mut model = engine.load_model()?;
    let mut embeddings: Vec<Vec<f32>> = Vec::new();
    let mut segment_indices: Vec<usize> = Vec::new();

    for (i, segment) in segments.iter().enumerate() {
        if RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst) {
            return Err(anyhow!("Retranscription cancelled"));
        }
        if segment.speaker.as_deref() != Some("system") {
            continue;
        }
        let (Some(start_s), Some(end_s)) = (segment.audio_start_time, segment.audio_end_time) else {
            continue;
        };
        let start = ((start_s * 16000.0) as usize).min(samples.len());
        let end = ((end_s * 16000.0) as usize).min(samples.len());
        if end <= start || end - start < 800 {
            continue; // too short for a meaningful embedding (< 50ms)
        }

        match model.extract_embedding(&samples[start..end]) {
            Ok(embedding) => {
                embeddings.push(embedding);
                segment_indices.push(i);
            }
            Err(e) => {
                warn!("Skipping embedding for segment {}: {}", segment.id, e);
            }
        }
    }

    if embeddings.len() < 2 {
        info!("Not enough system-audio segments for diarization ({})", embeddings.len());
        return Ok(0);
    }

    emit_progress(app, meeting_id, "diarizing", 95, "Clustering speakers...");
    let cluster_ids = crate::diarization_engine::cluster_embeddings(&embeddings);
    let distinct_clusters: std::collections::HashSet<usize> = cluster_ids.iter().copied().collect();
    if distinct_clusters.len() < 2 {
        info!("Diarization found only one distinct speaker in system audio - leaving as 'Others'");
        return Ok(0);
    }

    // Number speakers by chronological first appearance, not raw cluster id order.
    let mut first_seen: std::collections::HashMap<usize, f64> = std::collections::HashMap::new();
    for (&cluster_id, &seg_idx) in cluster_ids.iter().zip(&segment_indices) {
        let start_time = segments[seg_idx].audio_start_time.unwrap_or(0.0);
        first_seen
            .entry(cluster_id)
            .and_modify(|t| *t = t.min(start_time))
            .or_insert(start_time);
    }
    let mut ordered_clusters: Vec<usize> = distinct_clusters.into_iter().collect();
    ordered_clusters.sort_by(|a, b| {
        first_seen[a].partial_cmp(&first_seen[b]).unwrap_or(std::cmp::Ordering::Equal)
    });
    let speaker_number: std::collections::HashMap<usize, usize> = ordered_clusters
        .into_iter()
        .enumerate()
        .map(|(number, cluster_id)| (cluster_id, number + 1))
        .collect();

    for (&cluster_id, &seg_idx) in cluster_ids.iter().zip(&segment_indices) {
        segments[seg_idx].speaker_label = Some(format!("Speaker {}", speaker_number[&cluster_id]));
    }

    info!(
        "Diarization complete for meeting {}: {} distinct speakers across {} system segments",
        meeting_id,
        speaker_number.len(),
        embeddings.len()
    );

    Ok(speaker_number.len())
}

/// Emit progress event
fn emit_progress<R: Runtime>(
    app: &AppHandle<R>,
    meeting_id: &str,
    stage: &str,
    progress: u32,
    message: &str,
) {
    let _ = app.emit(
        "retranscription-progress",
        RetranscriptionProgress {
            meeting_id: meeting_id.to_string(),
            stage: stage.to_string(),
            progress_percentage: progress,
            message: message.to_string(),
        },
    );
}

/// Get or initialize the Whisper engine, auto-loading the model if needed
/// If `requested_model` is provided, ensures that specific model is loaded
async fn get_or_init_whisper<R: Runtime>(
    app: &AppHandle<R>,
    requested_model: Option<&str>,
) -> Result<Arc<WhisperEngine>> {
    use crate::whisper_engine::commands::WHISPER_ENGINE;

    let engine = {
        let guard = WHISPER_ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().cloned()
    };

    match engine {
        Some(e) => {
            // Determine which model to use
            let target_model = match requested_model {
                Some(model) => model.to_string(),
                None => get_configured_whisper_model(app).await?,
            };

            // Check if the correct model is already loaded
            let current_model = e.get_current_model().await;
            let needs_load = match &current_model {
                Some(loaded) => loaded != &target_model,
                None => true,
            };

            if needs_load {
                info!(
                    "Loading Whisper model '{}' (current: {:?})",
                    target_model, current_model
                );

                // Discover available models first (populates the internal cache)
                info!("Discovering available Whisper models...");
                if let Err(discover_err) = e.discover_models().await {
                    warn!("Error during model discovery (continuing anyway): {}", discover_err);
                }

                match e.load_model(&target_model).await {
                    Ok(_) => {
                        info!("Whisper model '{}' loaded successfully", target_model);
                        Ok(e)
                    }
                    Err(load_err) => {
                        error!("Failed to load Whisper model '{}': {}", target_model, load_err);
                        Err(anyhow!("Failed to load Whisper model '{}': {}", target_model, load_err))
                    }
                }
            } else {
                info!("Whisper model '{}' already loaded", target_model);
                Ok(e)
            }
        }
        None => Err(anyhow!("Whisper engine not initialized")),
    }
}

/// Get the configured Whisper model name from the database
async fn get_configured_whisper_model<R: Runtime>(app: &AppHandle<R>) -> Result<String> {
    debug!("Getting configured Whisper model from database...");

    let app_state = app
        .try_state::<AppState>()
        .ok_or_else(|| {
            error!("App state not available");
            anyhow!("App state not available")
        })?;

    debug!("Querying transcript_settings table...");

    // Query the transcript settings from the database - get both provider and model
    let result: Option<(String, String)> = sqlx::query_as(
        "SELECT provider, model FROM transcript_settings WHERE id = '1'"
    )
    .fetch_optional(app_state.db_manager.pool())
    .await
    .map_err(|e| {
        error!("Failed to query transcript config: {}", e);
        anyhow!("Failed to query transcript config: {}", e)
    })?;

    match result {
        Some((provider, model)) => {
            info!("Found transcript config: provider={}, model={}", provider, model);

            // Check if provider is Whisper-based
            if provider == "localWhisper" || provider == "whisper" {
                Ok(model)
            } else {
                error!("Retranscription requires Whisper provider, but configured provider is: {}", provider);
                Err(anyhow!("Retranscription requires Whisper. Current provider '{}' does not support retranscription with language selection.", provider))
            }
        },
        None => {
            // Default to configured Whisper model if no config exists
            warn!("No transcript config found, using default model '{}'", DEFAULT_WHISPER_MODEL);
            Ok(DEFAULT_WHISPER_MODEL.to_string())
        }
    }
}

/// Get or initialize the Parakeet engine, auto-loading the model if needed
async fn get_or_init_parakeet<R: Runtime>(
    app: &AppHandle<R>,
    requested_model: Option<&str>,
) -> Result<Arc<ParakeetEngine>> {
    use crate::parakeet_engine::commands::PARAKEET_ENGINE;

    let engine = {
        let guard = PARAKEET_ENGINE.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref().cloned()
    };

    match engine {
        Some(e) => {
            // Determine which model to use
            let target_model = match requested_model {
                Some(model) => model.to_string(),
                None => get_configured_parakeet_model(app).await?,
            };

            // Check if the correct model is already loaded
            let current_model = e.get_current_model().await;
            let needs_load = match &current_model {
                Some(loaded) => loaded != &target_model,
                None => true,
            };

            if needs_load {
                info!(
                    "Loading Parakeet model '{}' (current: {:?})",
                    target_model, current_model
                );

                // Discover available models first
                info!("Discovering available Parakeet models...");
                if let Err(discover_err) = e.discover_models().await {
                    warn!("Error during Parakeet model discovery (continuing anyway): {}", discover_err);
                }

                match e.load_model(&target_model).await {
                    Ok(_) => {
                        info!("Parakeet model '{}' loaded successfully", target_model);
                        Ok(e)
                    }
                    Err(load_err) => {
                        error!("Failed to load Parakeet model '{}': {}", target_model, load_err);
                        Err(anyhow!("Failed to load Parakeet model '{}': {}", target_model, load_err))
                    }
                }
            } else {
                info!("Parakeet model '{}' already loaded", target_model);
                Ok(e)
            }
        }
        None => Err(anyhow!("Parakeet engine not initialized")),
    }
}

/// Get the configured Parakeet model name from the database
async fn get_configured_parakeet_model<R: Runtime>(app: &AppHandle<R>) -> Result<String> {
    debug!("Getting configured Parakeet model from database...");

    let app_state = app
        .try_state::<AppState>()
        .ok_or_else(|| {
            error!("App state not available");
            anyhow!("App state not available")
        })?;

    // Query the transcript settings from the database
    let result: Option<(String, String)> = sqlx::query_as(
        "SELECT provider, model FROM transcript_settings WHERE id = '1'"
    )
    .fetch_optional(app_state.db_manager.pool())
    .await
    .map_err(|e| {
        error!("Failed to query transcript config: {}", e);
        anyhow!("Failed to query transcript config: {}", e)
    })?;

    match result {
        Some((provider, model)) => {
            info!("Found transcript config: provider={}, model={}", provider, model);

            if provider == "parakeet" {
                Ok(model)
            } else {
                // Default to configured Parakeet model
                warn!("Configured provider is not Parakeet, using default model");
                Ok(DEFAULT_PARAKEET_MODEL.to_string())
            }
        },
        None => {
            // Default to configured Parakeet model if no config exists
            warn!("No transcript config found, using default Parakeet model");
            Ok(DEFAULT_PARAKEET_MODEL.to_string())
        }
    }
}

/// Write or update metadata.json for retranscription (preserves existing fields, adds retranscribed_at)
fn write_retranscription_metadata(
    folder: &Path,
    meeting_id: &str,
    duration_seconds: f64,
    audio_filename: &str,
) -> Result<()> {
    let metadata_path = folder.join("metadata.json");
    let temp_path = folder.join(".metadata.json.tmp");
    let now = chrono::Utc::now().to_rfc3339();

    // Try to read existing metadata and update it
    let json = if metadata_path.exists() {
        let existing = std::fs::read_to_string(&metadata_path)?;
        let mut value: serde_json::Value = serde_json::from_str(&existing)?;
        if let Some(obj) = value.as_object_mut() {
            obj.insert("retranscribed_at".to_string(), serde_json::json!(now));
            obj.insert("status".to_string(), serde_json::json!("completed"));
            obj.insert("transcript_file".to_string(), serde_json::json!("transcripts.json"));
            obj.remove("detected_summary_language");
        }
        value
    } else {
        serde_json::json!({
            "version": "1.0",
            "meeting_id": meeting_id,
            "created_at": now,
            "completed_at": now,
            "retranscribed_at": now,
            "duration_seconds": duration_seconds,
            "audio_file": audio_filename,
            "transcript_file": "transcripts.json",
            "status": "completed",
            "source": "retranscription"
        })
    };

    let json_string = serde_json::to_string_pretty(&json)?;
    std::fs::write(&temp_path, &json_string)?;
    std::fs::rename(&temp_path, &metadata_path)?;

    info!("Wrote metadata.json to {}", metadata_path.display());
    Ok(())
}

// Tauri commands

/// Response when retranscription is started
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetranscriptionStarted {
    pub meeting_id: String,
    pub message: String,
}

// Start retranscription (Beta gated using configContext.betaFeatures)
#[tauri::command]
pub async fn start_retranscription_command<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
    language: Option<String>,
    model: Option<String>,
    provider: Option<String>,
) -> Result<RetranscriptionStarted, String> {

    // Check if retranscription is already in progress (guard will be acquired in start_retranscription)
    if RETRANSCRIPTION_IN_PROGRESS.load(Ordering::SeqCst) {
        return Err("Retranscription already in progress".to_string());
    }

    // Clone values for the spawned task
    let meeting_id_clone = meeting_id.clone();

    // Spawn the retranscription in a background task
    tauri::async_runtime::spawn(async move {
        let result = start_retranscription(
            app,
            meeting_id_clone,
            meeting_folder_path,
            language,
            model,
            provider,
        )
        .await;

        // Errors are already emitted as events in start_retranscription
        // so we just log here for debugging
        if let Err(e) = result {
            error!("Retranscription failed: {}", e);
        }
    });

    Ok(RetranscriptionStarted {
        meeting_id,
        message: "Retranscription started".to_string(),
    })
}

/// Response when speaker identification is started
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerIdentificationStarted {
    pub meeting_id: String,
    pub message: String,
}

// Start standalone speaker identification for an already-transcribed meeting.
// Shares the same in-progress guard as retranscription (see RETRANSCRIPTION_IN_PROGRESS)
// - only one background job of either kind runs at a time - so cancellation and status
// also reuse cancel_retranscription_command / is_retranscription_in_progress_command below.
#[tauri::command]
pub async fn start_speaker_identification_command<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    meeting_folder_path: String,
) -> Result<SpeakerIdentificationStarted, String> {
    if RETRANSCRIPTION_IN_PROGRESS.load(Ordering::SeqCst) {
        return Err("Another transcription/diarization job is already in progress".to_string());
    }

    let meeting_id_clone = meeting_id.clone();

    tauri::async_runtime::spawn(async move {
        let result = start_speaker_identification(app, meeting_id_clone, meeting_folder_path).await;

        // Errors are already emitted as events in start_speaker_identification
        // so we just log here for debugging
        if let Err(e) = result {
            error!("Speaker identification failed: {}", e);
        }
    });

    Ok(SpeakerIdentificationStarted {
        meeting_id,
        message: "Speaker identification started".to_string(),
    })
}

#[tauri::command]
pub async fn cancel_retranscription_command() -> Result<(), String> {
    if !is_retranscription_in_progress() {
        return Err("No retranscription in progress".to_string());
    }
    cancel_retranscription();
    Ok(())
}

#[tauri::command]
pub async fn is_retranscription_in_progress_command() -> bool {
    is_retranscription_in_progress()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_transcript_segments_empty() {
        let transcripts: Vec<(String, f64, f64)> = vec![];
        let segments = create_transcript_segments(&transcripts);
        assert!(segments.is_empty());
    }

    #[test]
    fn test_create_transcript_segments_single() {
        let transcripts = vec![
            ("Hello world".to_string(), 0.0, 1500.0), // 0-1.5 seconds
        ];
        let segments = create_transcript_segments(&transcripts);

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Hello world");
        assert_eq!(segments[0].audio_start_time, Some(0.0));
        assert_eq!(segments[0].audio_end_time, Some(1.5));
        assert_eq!(segments[0].duration, Some(1.5));
    }

    #[test]
    fn test_create_transcript_segments_multiple() {
        let transcripts = vec![
            ("First segment".to_string(), 0.0, 2000.0),      // 0-2 seconds
            ("Second segment".to_string(), 3000.0, 5000.0),  // 3-5 seconds
            ("Third segment".to_string(), 6500.0, 8000.0),   // 6.5-8 seconds
        ];
        let segments = create_transcript_segments(&transcripts);

        assert_eq!(segments.len(), 3);

        // First segment
        assert_eq!(segments[0].text, "First segment");
        assert_eq!(segments[0].audio_start_time, Some(0.0));
        assert_eq!(segments[0].audio_end_time, Some(2.0));
        assert_eq!(segments[0].duration, Some(2.0));

        // Second segment
        assert_eq!(segments[1].text, "Second segment");
        assert_eq!(segments[1].audio_start_time, Some(3.0));
        assert_eq!(segments[1].audio_end_time, Some(5.0));
        assert_eq!(segments[1].duration, Some(2.0));

        // Third segment
        assert_eq!(segments[2].text, "Third segment");
        assert_eq!(segments[2].audio_start_time, Some(6.5));
        assert_eq!(segments[2].audio_end_time, Some(8.0));
        assert_eq!(segments[2].duration, Some(1.5));
    }

    #[test]
    fn test_create_transcript_segments_trims_whitespace() {
        let transcripts = vec![
            ("  Hello with spaces  ".to_string(), 0.0, 1000.0),
        ];
        let segments = create_transcript_segments(&transcripts);

        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Hello with spaces");
    }

    #[test]
    fn test_create_transcript_segments_generates_unique_ids() {
        let transcripts = vec![
            ("Segment one".to_string(), 0.0, 1000.0),
            ("Segment two".to_string(), 1000.0, 2000.0),
        ];
        let segments = create_transcript_segments(&transcripts);

        assert_eq!(segments.len(), 2);
        assert_ne!(segments[0].id, segments[1].id);
        assert!(segments[0].id.starts_with("transcript-"));
        assert!(segments[1].id.starts_with("transcript-"));
    }

    #[test]
    fn test_cancellation_flag() {
        // Reset flag to known state
        RETRANSCRIPTION_CANCELLED.store(false, Ordering::SeqCst);
        RETRANSCRIPTION_IN_PROGRESS.store(false, Ordering::SeqCst);

        assert!(!is_retranscription_in_progress());

        // Test cancellation
        cancel_retranscription();
        assert!(RETRANSCRIPTION_CANCELLED.load(Ordering::SeqCst));

        // Reset for other tests
        RETRANSCRIPTION_CANCELLED.store(false, Ordering::SeqCst);
    }

    #[test]
    fn test_vad_redemption_time_constant() {
        // Batch processing uses 2000ms to bridge natural pauses in full-file VAD
        assert_eq!(VAD_REDEMPTION_TIME_MS, 2000);
    }

    #[test]
    fn test_find_audio_file_common_candidates() {
        let dir = tempfile::tempdir().unwrap();

        // No audio file → error
        assert!(find_audio_file(dir.path()).is_err());

        // Create audio.mp4 — should be found first
        std::fs::write(dir.path().join("audio.mp4"), b"fake").unwrap();
        let found = find_audio_file(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "audio.mp4");
    }

    #[test]
    fn test_find_audio_file_non_mp4_extensions() {
        let dir = tempfile::tempdir().unwrap();

        // Create audio.wav (imported as .wav, not .mp4)
        std::fs::write(dir.path().join("audio.wav"), b"fake").unwrap();
        let found = find_audio_file(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "audio.wav");
    }

    #[test]
    fn test_find_audio_file_fallback_scan() {
        let dir = tempfile::tempdir().unwrap();

        // Create a file with an audio extension but non-standard name
        std::fs::write(dir.path().join("my_recording.flac"), b"fake").unwrap();
        // Also add a non-audio file that should be ignored
        std::fs::write(dir.path().join("notes.txt"), b"text").unwrap();

        let found = find_audio_file(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "my_recording.flac");
    }

    #[test]
    fn test_find_audio_file_priority_order() {
        let dir = tempfile::tempdir().unwrap();

        // Create both audio.m4a and audio.mp4 — mp4 should win (listed first in candidates)
        std::fs::write(dir.path().join("audio.m4a"), b"fake").unwrap();
        std::fs::write(dir.path().join("audio.mp4"), b"fake").unwrap();
        let found = find_audio_file(dir.path()).unwrap();
        assert_eq!(found.file_name().unwrap(), "audio.mp4");
    }

    #[test]
    fn test_find_audio_file_empty_folder() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_audio_file(dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No audio file found"));
    }

    #[test]
    fn test_find_audio_file_nonexistent_folder() {
        let result = find_audio_file(Path::new("/nonexistent/path/12345"));
        assert!(result.is_err());
    }

    #[test]
    fn test_audio_extensions_constant() {
        // Verify all expected formats are covered
        assert!(AUDIO_EXTENSIONS.contains(&"mp4"));
        assert!(AUDIO_EXTENSIONS.contains(&"m4a"));
        assert!(AUDIO_EXTENSIONS.contains(&"wav"));
        assert!(AUDIO_EXTENSIONS.contains(&"mp3"));
        assert!(AUDIO_EXTENSIONS.contains(&"flac"));
        assert!(AUDIO_EXTENSIONS.contains(&"ogg"));
        assert!(AUDIO_EXTENSIONS.contains(&"aac"));
        // FFmpeg-backed formats
        assert!(AUDIO_EXTENSIONS.contains(&"mkv"));
        assert!(AUDIO_EXTENSIONS.contains(&"webm"));
        assert!(AUDIO_EXTENSIONS.contains(&"wma"));
        // Non-audio formats
        assert!(!AUDIO_EXTENSIONS.contains(&"txt"));
        assert!(!AUDIO_EXTENSIONS.contains(&"pdf"));
    }
}
