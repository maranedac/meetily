-- Migration: Add transcription_status to meetings
-- Supports "record only" mode: a meeting can be saved as audio-only (no live
-- transcription) and transcribed later on demand from meeting-details.
-- Values: 'completed' (has a transcript - the default, matches all existing rows
-- and the normal live-transcribed flow) | 'pending' (audio-only, not yet transcribed)

ALTER TABLE meetings ADD COLUMN transcription_status TEXT NOT NULL DEFAULT 'completed';
