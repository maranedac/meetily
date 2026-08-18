use super::engine::{DiarizationEngine, DiarizationModelInfo, DownloadProgress};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tauri::{command, AppHandle, Emitter, Manager, Runtime};

pub static DIARIZATION_ENGINE: Mutex<Option<Arc<DiarizationEngine>>> = Mutex::new(None);
static MODELS_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Mirrors `parakeet_engine::commands::set_models_directory` / `whisper_engine`'s -
/// same `app_data_dir()/models` base, own `diarization` subdirectory (see
/// `DiarizationEngine::new_with_models_dir`).
pub fn set_models_directory<R: Runtime>(app: &AppHandle<R>) {
    let Ok(app_data_dir) = app.path().app_data_dir() else {
        log::error!("Failed to get app data dir for diarization models");
        return;
    };
    let models_dir = app_data_dir.join("models");
    if !models_dir.exists() {
        if let Err(e) = std::fs::create_dir_all(&models_dir) {
            log::error!("Failed to create models directory: {}", e);
            return;
        }
    }
    *MODELS_DIR.lock().unwrap() = Some(models_dir);
}

fn get_models_directory() -> Option<PathBuf> {
    MODELS_DIR.lock().unwrap().clone()
}

pub async fn diarization_init() -> Result<(), String> {
    let mut guard = DIARIZATION_ENGINE.lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }
    let models_dir = get_models_directory();
    let engine = DiarizationEngine::new_with_models_dir(models_dir)
        .map_err(|e| format!("Failed to initialize diarization engine: {}", e))?;
    *guard = Some(Arc::new(engine));
    Ok(())
}

fn get_engine() -> Option<Arc<DiarizationEngine>> {
    DIARIZATION_ENGINE.lock().unwrap().as_ref().cloned()
}

#[command]
pub async fn diarization_get_available_models() -> Result<DiarizationModelInfo, String> {
    diarization_init().await?;
    let engine = get_engine().ok_or("Diarization engine not initialized")?;
    Ok(engine.get_available_models().await)
}

#[command]
pub async fn diarization_is_model_ready() -> Result<bool, String> {
    diarization_init().await?;
    let engine = get_engine().ok_or("Diarization engine not initialized")?;
    Ok(engine.is_model_ready().await)
}

#[command]
pub async fn diarization_download_model<R: Runtime>(app_handle: AppHandle<R>) -> Result<(), String> {
    diarization_init().await?;
    let engine = get_engine().ok_or("Diarization engine not initialized")?;

    let app_for_progress = app_handle.clone();
    let progress_callback: Box<dyn Fn(DownloadProgress) + Send> = Box::new(move |progress| {
        let _ = app_for_progress.emit("diarization-model-download-progress", &progress);
    });

    let result = engine.download_model(Some(progress_callback)).await;

    match &result {
        Ok(()) => {
            let _ = app_handle.emit("diarization-model-download-complete", ());
        }
        Err(e) => {
            let _ = app_handle.emit(
                "diarization-model-download-error",
                serde_json::json!({ "error": e.to_string() }),
            );
        }
    }

    result.map_err(|e| e.to_string())
}

#[command]
pub async fn diarization_cancel_download() -> Result<(), String> {
    let engine = get_engine().ok_or("Diarization engine not initialized")?;
    engine.cancel_download().await.map_err(|e| e.to_string())
}

#[command]
pub async fn diarization_delete_model() -> Result<(), String> {
    let engine = get_engine().ok_or("Diarization engine not initialized")?;
    engine.delete_model().await.map_err(|e| e.to_string())
}
