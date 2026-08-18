'use client';

import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Download, Trash2, CheckCircle2, Users, Loader2, AlertCircle } from 'lucide-react';
import { Button } from './ui/button';
import { toast } from 'sonner';
import Analytics from '@/lib/analytics';

type ModelStatus =
  | 'Available'
  | 'Missing'
  | { Downloading: { progress: number } }
  | { Error: string }
  | { Corrupted: { file_size: number; expected_min_size: number } };

interface DiarizationModelInfo {
  name: string;
  size_mb: number;
  description: string;
  status: ModelStatus;
}

interface DownloadProgress {
  downloaded_mb: number;
  total_mb: number;
  speed_mbps: number;
  percent: number;
}

function isDownloading(status: ModelStatus): boolean {
  return typeof status === 'object' && 'Downloading' in status;
}
function isCorrupted(status: ModelStatus): boolean {
  return typeof status === 'object' && 'Corrupted' in status;
}
function errorMessage(status: ModelStatus): string | null {
  return typeof status === 'object' && 'Error' in status ? status.Error : null;
}

/**
 * Settings panel for the offline speaker-diarization embedding model
 * (WeSpeaker ResNet34, ~25MB). Separate from Whisper/Parakeet - this model
 * identifies distinct speakers ("Speaker 1", "Speaker 2"...) within the "system"
 * audio track of "record only" meetings, run automatically by the "Transcribe"
 * action once downloaded. Not required for normal transcription - if it's never
 * downloaded, diarization is silently skipped and segments show as "Others".
 */
export function DiarizationModelManager() {
  const [model, setModel] = useState<DiarizationModelInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const lastProgressUpdateRef = useRef(0);

  const fetchStatus = useCallback(async () => {
    try {
      const info = await invoke<DiarizationModelInfo>('diarization_get_available_models');
      setModel(info);
    } catch (error) {
      console.error('Failed to fetch diarization model status:', error);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchStatus();
  }, [fetchStatus]);

  useEffect(() => {
    const unlisteners: Array<() => void> = [];

    listen<DownloadProgress>('diarization-model-download-progress', (event) => {
      // Throttle to ~5 updates/sec, same rationale as ParakeetModelManager
      const now = Date.now();
      if (now - lastProgressUpdateRef.current < 200) return;
      lastProgressUpdateRef.current = now;
      setProgress(event.payload);
    }).then((fn) => unlisteners.push(fn));

    listen('diarization-model-download-complete', () => {
      setProgress(null);
      toast.success('Speaker identification model downloaded');
      fetchStatus();
    }).then((fn) => unlisteners.push(fn));

    listen<{ error: string }>('diarization-model-download-error', (event) => {
      setProgress(null);
      toast.error('Failed to download speaker identification model', {
        description: event.payload.error,
      });
      fetchStatus();
    }).then((fn) => unlisteners.push(fn));

    return () => unlisteners.forEach((fn) => fn());
  }, [fetchStatus]);

  const handleDownload = async () => {
    Analytics.trackButtonClick('download_diarization_model', 'settings');
    try {
      setProgress({ downloaded_mb: 0, total_mb: model?.size_mb ?? 0, speed_mbps: 0, percent: 0 });
      await invoke('diarization_download_model');
    } catch (error) {
      setProgress(null);
      toast.error('Failed to start download', {
        description: error instanceof Error ? error.message : String(error),
      });
    }
  };

  const handleCancel = async () => {
    try {
      await invoke('diarization_cancel_download');
      setProgress(null);
      toast.info('Download cancelled');
      fetchStatus();
    } catch (error) {
      console.error('Failed to cancel download:', error);
    }
  };

  const handleDelete = async () => {
    try {
      await invoke('diarization_delete_model');
      toast.success('Speaker identification model removed');
      fetchStatus();
    } catch (error) {
      toast.error('Failed to remove model', {
        description: error instanceof Error ? error.message : String(error),
      });
    }
  };

  if (loading) {
    return <div className="animate-pulse h-20 bg-gray-100 rounded-lg" />;
  }
  if (!model) {
    return null;
  }

  const downloading = isDownloading(model.status) || progress !== null;
  const available = model.status === 'Available';
  const corrupted = isCorrupted(model.status);
  const error = errorMessage(model.status);

  return (
    <div className="border rounded-lg p-4 bg-gray-50">
      <div className="flex items-start justify-between gap-4">
        <div className="flex items-start gap-3">
          <Users className="w-5 h-5 text-gray-500 mt-0.5" />
          <div>
            <div className="font-medium flex items-center gap-2">
              Speaker Identification
              {available && <CheckCircle2 className="w-4 h-4 text-green-600" />}
            </div>
            <p className="text-sm text-gray-600 mt-0.5">{model.description}</p>
            <p className="text-xs text-gray-400 mt-1">
              {model.size_mb} MB · Runs automatically when you press "Transcribe" on a
              "record only" meeting - optional, skipped if not downloaded.
            </p>
          </div>
        </div>

        <div className="flex-shrink-0">
          {available ? (
            <Button variant="outline" size="sm" onClick={handleDelete}>
              <Trash2 className="w-4 h-4 mr-1.5" />
              Remove
            </Button>
          ) : downloading ? (
            <Button variant="outline" size="sm" onClick={handleCancel}>
              <Loader2 className="w-4 h-4 mr-1.5 animate-spin" />
              Cancel
            </Button>
          ) : (
            <Button variant="outline" size="sm" onClick={handleDownload}>
              <Download className="w-4 h-4 mr-1.5" />
              {corrupted ? 'Re-download' : 'Download'}
            </Button>
          )}
        </div>
      </div>

      {downloading && progress && (
        <div className="mt-3">
          <div className="w-full bg-gray-200 rounded-full h-2">
            <div
              className="bg-blue-600 h-2 rounded-full transition-all duration-300"
              style={{ width: `${Math.min(progress.percent, 100)}%` }}
            />
          </div>
          <div className="flex justify-between text-xs text-gray-500 mt-1">
            <span>{progress.downloaded_mb.toFixed(1)} / {progress.total_mb.toFixed(1)} MB</span>
            <span>{progress.speed_mbps.toFixed(1)} MB/s</span>
          </div>
        </div>
      )}

      {(error || corrupted) && (
        <div className="mt-3 flex items-center gap-2 text-sm text-red-700 bg-red-50 border border-red-200 rounded-md px-3 py-2">
          <AlertCircle className="w-4 h-4 flex-shrink-0" />
          <span>{error || 'Downloaded file appears incomplete or corrupted - please re-download.'}</span>
        </div>
      )}
    </div>
  );
}
