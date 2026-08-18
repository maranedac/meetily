//! Speaker-embedding model download/lifecycle management.
//!
//! Mirrors `parakeet_engine/parakeet_engine.rs`'s structure (own models directory,
//! own download logic, own Tauri-facing progress events) since this codebase doesn't
//! have a shared/generic model manager - each ONNX-based engine is self-contained.
//! Simplified relative to Parakeet's downloader: this is a single ~25MB file (not 4
//! files totaling 650MB+), so resume-from-partial-byte-range support wasn't worth
//! the added complexity - a fresh download is fast enough to just retry on failure.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

use super::model::DiarizationModel;

/// Single model for now - a small, well-established English/VoxCeleb WeSpeaker
/// embedding model. Not gated behind a HuggingFace login (unlike pyannote/embedding),
/// which matters for silent auto-download UX parity with Whisper/Parakeet.
pub const MODEL_NAME: &str = "wespeaker-en-voxceleb-resnet34";
const MODEL_FILENAME: &str = "wespeaker_en_voxceleb_resnet34.onnx";
const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34.onnx";
const MODEL_SIZE_BYTES: u64 = 26_534_365; // ~25.3MB, from the GitHub release listing
const MODEL_MIN_VALID_SIZE_BYTES: u64 = (MODEL_SIZE_BYTES as f64 * 0.9) as u64; // corruption/partial-download guard

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelStatus {
    Available,
    Missing,
    Downloading { progress: u8 },
    Error(String),
    Corrupted { file_size: u64, expected_min_size: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarizationModelInfo {
    pub name: String,
    pub size_mb: u32,
    pub description: String,
    pub status: ModelStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub downloaded_mb: f64,
    pub total_mb: f64,
    pub speed_mbps: f64,
    pub percent: u8,
}

impl DownloadProgress {
    fn new(downloaded: u64, total: u64, speed_mbps: f64) -> Self {
        let percent = if total > 0 {
            ((downloaded as f64 / total as f64) * 100.0).min(100.0) as u8
        } else {
            0
        };
        Self {
            downloaded_bytes: downloaded,
            total_bytes: total,
            downloaded_mb: downloaded as f64 / (1024.0 * 1024.0),
            total_mb: total as f64 / (1024.0 * 1024.0),
            speed_mbps,
            percent,
        }
    }
}

pub struct DiarizationEngine {
    models_dir: PathBuf,
    cancel_flag: RwLock<Option<String>>,
    active_downloads: RwLock<HashSet<String>>,
}

impl DiarizationEngine {
    pub fn new_with_models_dir(models_dir: Option<PathBuf>) -> Result<Self> {
        let models_dir = if let Some(dir) = models_dir {
            dir.join("diarization")
        } else {
            let current_dir = std::env::current_dir()
                .map_err(|e| anyhow!("Failed to get current directory: {}", e))?;
            if cfg!(debug_assertions) {
                current_dir.join("models").join("diarization")
            } else {
                dirs::data_dir()
                    .or_else(dirs::home_dir)
                    .ok_or_else(|| anyhow!("Could not find system data directory"))?
                    .join("Meetily")
                    .join("models")
                    .join("diarization")
            }
        };

        if !models_dir.exists() {
            std::fs::create_dir_all(&models_dir)?;
        }

        log::info!("DiarizationEngine using models directory: {}", models_dir.display());

        Ok(Self {
            models_dir,
            cancel_flag: RwLock::new(None),
            active_downloads: RwLock::new(HashSet::new()),
        })
    }

    fn model_path(&self) -> PathBuf {
        self.models_dir.join(MODEL_FILENAME)
    }

    pub async fn get_available_models(&self) -> DiarizationModelInfo {
        let path = self.model_path();
        let is_downloading = self.active_downloads.read().await.contains(MODEL_NAME);

        let status = if is_downloading {
            ModelStatus::Downloading { progress: 0 }
        } else if path.exists() {
            match std::fs::metadata(&path) {
                Ok(meta) if meta.len() >= MODEL_MIN_VALID_SIZE_BYTES => ModelStatus::Available,
                Ok(meta) => ModelStatus::Corrupted {
                    file_size: meta.len(),
                    expected_min_size: MODEL_MIN_VALID_SIZE_BYTES,
                },
                Err(_) => ModelStatus::Missing,
            }
        } else {
            ModelStatus::Missing
        };

        DiarizationModelInfo {
            name: MODEL_NAME.to_string(),
            size_mb: (MODEL_SIZE_BYTES / (1024 * 1024)) as u32,
            description: "WeSpeaker ResNet34 (English, VoxCeleb) - identifies distinct \
                speakers within the 'system' audio track for offline diarization."
                .to_string(),
            status,
        }
    }

    pub async fn is_model_ready(&self) -> bool {
        matches!(self.get_available_models().await.status, ModelStatus::Available)
    }

    /// Load the embedding model, downloading it first is the CALLER's responsibility
    /// (diarization is best-effort: callers should check `is_model_ready()` and skip
    /// diarization entirely rather than force a download mid-transcription).
    pub fn load_model(&self) -> Result<DiarizationModel> {
        DiarizationModel::new(self.model_path())
    }

    pub async fn delete_model(&self) -> Result<()> {
        let path = self.model_path();
        if path.exists() {
            fs::remove_file(&path).await
                .map_err(|e| anyhow!("Failed to delete diarization model: {}", e))?;
        }
        Ok(())
    }

    pub async fn cancel_download(&self) -> Result<()> {
        *self.cancel_flag.write().await = Some(MODEL_NAME.to_string());
        self.active_downloads.write().await.remove(MODEL_NAME);

        // Brief delay to let the download loop observe the flag and exit, then clean
        // up the partial file so a retry starts fresh (no resume support, see header).
        tokio::time::sleep(Duration::from_millis(100)).await;
        let path = self.model_path();
        if path.exists() {
            let _ = fs::remove_file(&path).await;
        }
        Ok(())
    }

    pub async fn download_model(
        &self,
        progress_callback: Option<Box<dyn Fn(DownloadProgress) + Send>>,
    ) -> Result<()> {
        {
            let active = self.active_downloads.read().await;
            if active.contains(MODEL_NAME) {
                return Err(anyhow!("Download already in progress"));
            }
        }
        self.active_downloads.write().await.insert(MODEL_NAME.to_string());
        *self.cancel_flag.write().await = None;

        let result = self.download_model_inner(progress_callback).await;

        self.active_downloads.write().await.remove(MODEL_NAME);
        if result.is_err() {
            // Leave partial file for a human to inspect only if it's suspiciously
            // large; otherwise clean up so `get_available_models` reports Missing,
            // not Corrupted, for a simple network-hiccup failure.
            let path = self.model_path();
            if let Ok(meta) = std::fs::metadata(&path) {
                if meta.len() < MODEL_MIN_VALID_SIZE_BYTES {
                    let _ = fs::remove_file(&path).await;
                }
            }
        }
        result
    }

    async fn download_model_inner(
        &self,
        progress_callback: Option<Box<dyn Fn(DownloadProgress) + Send>>,
    ) -> Result<()> {
        let path = self.model_path();
        log::info!("Downloading diarization embedding model from {}", MODEL_URL);

        let client = reqwest::Client::builder()
            .tcp_nodelay(true)
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        let response = client.get(MODEL_URL).send().await
            .map_err(|e| anyhow!("Failed to start download: {}", e))?;
        if !response.status().is_success() {
            return Err(anyhow!("Download failed with status: {}", response.status()));
        }
        let total_bytes = response.content_length().unwrap_or(MODEL_SIZE_BYTES);

        let file = fs::File::create(&path).await
            .map_err(|e| anyhow!("Failed to create model file: {}", e))?;
        let mut writer = tokio::io::BufWriter::with_capacity(1024 * 1024, file);

        use futures_util::StreamExt;
        let mut stream = response.bytes_stream();
        let mut downloaded: u64 = 0;
        let start_time = Instant::now();
        let mut last_report = Instant::now();
        let mut bytes_since_report: u64 = 0;

        while let Some(chunk_result) = stream.next().await {
            if self.cancel_flag.read().await.as_deref() == Some(MODEL_NAME) {
                let _ = writer.flush().await;
                return Err(anyhow!("Download cancelled by user"));
            }

            let chunk = chunk_result.map_err(|e| anyhow!("Download error: {}", e))?;
            writer.write_all(&chunk).await
                .map_err(|e| anyhow!("Failed to write chunk: {}", e))?;

            downloaded += chunk.len() as u64;
            bytes_since_report += chunk.len() as u64;

            let elapsed = last_report.elapsed();
            if elapsed >= Duration::from_millis(300) {
                let speed_mbps = (bytes_since_report as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64().max(0.001);
                if let Some(ref cb) = progress_callback {
                    cb(DownloadProgress::new(downloaded, total_bytes, speed_mbps));
                }
                last_report = Instant::now();
                bytes_since_report = 0;
            }
        }

        writer.flush().await.map_err(|e| anyhow!("Failed to flush model file: {}", e))?;

        let total_elapsed = start_time.elapsed().as_secs_f64().max(0.001);
        let final_speed = (downloaded as f64 / (1024.0 * 1024.0)) / total_elapsed;
        if let Some(ref cb) = progress_callback {
            cb(DownloadProgress::new(downloaded, total_bytes, final_speed));
        }

        if downloaded < MODEL_MIN_VALID_SIZE_BYTES {
            return Err(anyhow!(
                "Downloaded file too small ({} bytes, expected at least {}) - likely an incomplete or failed download",
                downloaded, MODEL_MIN_VALID_SIZE_BYTES
            ));
        }

        log::info!("Diarization embedding model download complete: {} bytes", downloaded);
        Ok(())
    }
}
