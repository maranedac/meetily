-- Migration: Add speaker_label to transcripts (Phase 2 diarization)
-- Additive column, does NOT change the existing `speaker` column (still just
-- "mic"/"system"). Only populated for `speaker = 'system'` rows in meetings where
-- offline diarization ran (see audio/diarization_engine.rs) - "Speaker 1", "Speaker 2",
-- etc. NULL for everything else (all existing rows, mic rows, and any meeting where
-- diarization didn't run or the embedding model wasn't downloaded).

ALTER TABLE transcripts ADD COLUMN speaker_label TEXT;
