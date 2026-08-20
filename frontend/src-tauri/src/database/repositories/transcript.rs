use crate::api::{TranscriptSearchResult, TranscriptSegment};
use chrono::Utc;
use sqlx::{Connection, Error as SqlxError, SqlitePool};
use tracing::{error, info};
use uuid::Uuid;

pub struct TranscriptsRepository;

impl TranscriptsRepository {
    /// Saves a new meeting and its associated transcript segments.
    /// This function uses a transaction to ensure that either both the meeting
    /// and all its transcripts are saved, or none of them are.
    /// * `transcription_status` - "completed" (default, when `None`) for the normal flow
    ///   where transcripts are provided; pass `Some("pending")` when saving a "record only"
    ///   meeting (empty `transcripts`) that will be transcribed later on demand.
    pub async fn save_transcript(
        pool: &SqlitePool,
        meeting_title: &str,
        transcripts: &[TranscriptSegment],
        folder_path: Option<String>,
        transcription_status: Option<String>,
    ) -> Result<String, SqlxError> {
        let meeting_id = format!("meeting-{}", Uuid::new_v4());
        let transcription_status = transcription_status.unwrap_or_else(|| "completed".to_string());

        let mut conn = pool.acquire().await?;
        let mut transaction = conn.begin().await?;

        let now = Utc::now();

        // 1. Create the new meeting
        let result = sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at, folder_path, transcription_status) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&meeting_id)
        .bind(meeting_title)
        .bind(now)
        .bind(now)
        .bind(&folder_path)
        .bind(&transcription_status)
        .execute(&mut *transaction)
        .await;

        if let Err(e) = result {
            error!("Failed to create meeting '{}': {}", meeting_title, e);
            transaction.rollback().await?;
            return Err(e);
        }

        info!("Successfully created meeting with id: {}", meeting_id);

        // 2. Save each transcript segment with audio timing fields
        for segment in transcripts {
            let transcript_id = format!("transcript-{}", Uuid::new_v4());
            let result = sqlx::query(
                "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration, speaker)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(&transcript_id)
            .bind(&meeting_id)
            .bind(&segment.text)
            .bind(&segment.timestamp)
            .bind(segment.audio_start_time)
            .bind(segment.audio_end_time)
            .bind(segment.duration)
            .bind(&segment.speaker)
            .execute(&mut *transaction)
            .await;

            if let Err(e) = result {
                error!(
                    "Failed to save transcript segment for meeting {}: {}",
                    meeting_id, e
                );
                transaction.rollback().await?;
                return Err(e);
            }
        }

        info!(
            "Successfully saved {} transcript segments for meeting {}",
            transcripts.len(),
            meeting_id
        );

        // Commit the transaction
        transaction.commit().await?;

        Ok(meeting_id)
    }

    /// Searches for a query string within the transcripts.
    /// It returns a list of matching transcripts with context.
    pub async fn search_transcripts(
        pool: &SqlitePool,
        query: &str,
    ) -> Result<Vec<TranscriptSearchResult>, SqlxError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let search_query = format!("%{}%", query.to_lowercase());

        let rows = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT m.id, m.title, t.transcript, t.timestamp
             FROM meetings m
             JOIN transcripts t ON m.id = t.meeting_id
             WHERE LOWER(t.transcript) LIKE ?",
        )
        .bind(&search_query)
        .fetch_all(pool)
        .await?;

        let results = rows
            .into_iter()
            .map(|(id, title, transcript, timestamp)| {
                let match_context = Self::get_match_context(&transcript, query);
                TranscriptSearchResult {
                    id,
                    title,
                    match_context,
                    timestamp,
                }
            })
            .collect();

        Ok(results)
    }

    /// Renames every transcript segment sharing one speaker identity within a meeting
    /// (e.g. all "Speaker 1" system segments, or all "mic" segments) to a real name.
    ///
    /// `old_label` identifies which group to rename: `Some("Speaker 1")` targets rows
    /// with that exact `speaker_label` (from diarization); `None` targets rows with no
    /// label yet - the generic "You" (speaker="mic") or "Others" (speaker="system",
    /// never diarized / diarization found only one voice) buckets. `new_label` is
    /// stored as the new `speaker_label`, so this reuses the existing column and the
    /// existing frontend rendering (SpeakerBadge already prefers speaker_label when
    /// present) - no schema change needed.
    pub async fn rename_speaker(
        pool: &SqlitePool,
        meeting_id: &str,
        speaker: &str,
        old_label: Option<&str>,
        new_label: &str,
    ) -> Result<u64, SqlxError> {
        let result = match old_label {
            Some(old) => {
                sqlx::query(
                    "UPDATE transcripts SET speaker_label = ? WHERE meeting_id = ? AND speaker = ? AND speaker_label = ?"
                )
                .bind(new_label)
                .bind(meeting_id)
                .bind(speaker)
                .bind(old)
                .execute(pool)
                .await?
            }
            None => {
                sqlx::query(
                    "UPDATE transcripts SET speaker_label = ? WHERE meeting_id = ? AND speaker = ? AND speaker_label IS NULL"
                )
                .bind(new_label)
                .bind(meeting_id)
                .bind(speaker)
                .execute(pool)
                .await?
            }
        };

        info!(
            "Renamed speaker '{}' ({:?} -> {}) for meeting {}: {} rows updated",
            speaker, old_label, new_label, meeting_id, result.rows_affected()
        );

        Ok(result.rows_affected())
    }

    /// Helper function to extract a snippet of text around the first match of a query.
    fn get_match_context(transcript: &str, query: &str) -> String {
        let transcript_lower = transcript.to_lowercase();
        let query_lower = query.to_lowercase();

        match transcript_lower.find(&query_lower) {
            Some(match_index) => {
                let start_index = match_index.saturating_sub(100);
                let end_index = (match_index + query.len() + 100).min(transcript.len());

                let mut context = String::new();
                if start_index > 0 {
                    context.push_str("...");
                }
                context.push_str(&transcript[start_index..end_index]);
                if end_index < transcript.len() {
                    context.push_str("...");
                }
                context
            }
            None => transcript.chars().take(200).collect(), // Fallback to the start of the transcript
        }
    }
}
