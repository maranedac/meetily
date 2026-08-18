//! Offline speaker diarization ("Speaker 1"/"Speaker 2"/...) within the "system"
//! audio track of "record only" meetings.
//!
//! Phase 2 of speaker attribution (Phase 1 was the "mic"/"system" split). Runs
//! automatically as the last step of the "Transcribe" action (see
//! `audio/retranscription.rs`) for meetings that have separate `mic.*`/`system.*`
//! tracks. Always best-effort - never fails the transcription itself.
//!
//! # Module structure
//! - `engine`: model download/lifecycle management (mirrors `parakeet_engine`)
//! - `model`: ONNX embedding extraction (fbank features -> fixed-size vector)
//! - `cluster`: plain agglomerative clustering over embeddings, no new dependency
//! - `commands`: Tauri command interface for the Settings model-download panel

pub mod cluster;
pub mod commands;
pub mod engine;
pub mod model;

pub use cluster::cluster_embeddings;
pub use engine::{
    DiarizationEngine, DiarizationModelInfo, DownloadProgress, ModelStatus, MODEL_NAME,
};
pub use model::DiarizationModel;
