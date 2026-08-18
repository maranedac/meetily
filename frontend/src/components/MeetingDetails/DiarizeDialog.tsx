import React, { useState, useEffect, useRef } from 'react';
import { Users, Loader2, AlertCircle, X } from 'lucide-react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { Button } from '../ui/button';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { toast } from 'sonner';
import Analytics from '@/lib/analytics';

interface DiarizeDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  meetingId: string;
  meetingFolderPath: string | null;
  onComplete?: () => void;
}

interface DiarizationProgress {
  meeting_id: string;
  stage: string;
  progress_percentage: number;
  message: string;
}

interface SpeakerIdentificationResult {
  meeting_id: string;
  speakers_found: number;
  segments_updated: number;
}

interface DiarizationError {
  meeting_id: string;
  error: string;
}

// Runs the diarization engine (WeSpeaker embeddings + clustering, see
// diarization_engine/) against a meeting's EXISTING transcript rows - no
// re-transcription, nothing to configure, just a "system" audio track to analyze.
// Reuses the same backend job guard as RetranscribeDialog (only one background
// transcription/diarization job runs at a time), so cancel_retranscription_command
// works here too.
export function DiarizeDialog({
  open,
  onOpenChange,
  meetingId,
  meetingFolderPath,
  onComplete,
}: DiarizeDialogProps) {
  const [isProcessing, setIsProcessing] = useState(false);
  const [progress, setProgress] = useState<DiarizationProgress | null>(null);
  const [error, setError] = useState<string | null>(null);

  const onCompleteRef = useRef(onComplete);
  const onOpenChangeRef = useRef(onOpenChange);
  useEffect(() => { onCompleteRef.current = onComplete; }, [onComplete]);
  useEffect(() => { onOpenChangeRef.current = onOpenChange; }, [onOpenChange]);

  const prevOpenRef = useRef(false);

  useEffect(() => {
    const wasOpen = prevOpenRef.current;
    prevOpenRef.current = open;

    if (open && !wasOpen) {
      setIsProcessing(false);
      setProgress(null);
      setError(null);
    }
  }, [open]);

  // Not gated on `open` - a job started here can keep running after the dialog is
  // closed (same background-capable pattern as RetranscribeDialog), so the
  // completion toast still fires even after the user has moved on.
  useEffect(() => {
    const unlisteners: UnlistenFn[] = [];
    const cleanedUpRef = { current: false };

    const setupListeners = async () => {
      const unlistenProgress = await listen<DiarizationProgress>(
        'diarization-progress',
        (event) => {
          if (event.payload.meeting_id === meetingId) {
            setProgress(event.payload);
          }
        }
      );
      if (cleanedUpRef.current) {
        unlistenProgress();
        return;
      }
      unlisteners.push(unlistenProgress);

      const unlistenComplete = await listen<SpeakerIdentificationResult>(
        'diarization-complete',
        async (event) => {
          if (event.payload.meeting_id === meetingId) {
            await Analytics.track('identify_speakers_completed', {
              success: 'true',
              speakers_found: event.payload.speakers_found.toString(),
              segments_updated: event.payload.segments_updated.toString(),
            });

            setIsProcessing(false);
            toast.success(
              event.payload.speakers_found > 0
                ? `Identified ${event.payload.speakers_found} speaker${event.payload.speakers_found === 1 ? '' : 's'}.`
                : 'No distinct speakers found in system audio.'
            );
            onCompleteRef.current?.();
            onOpenChangeRef.current(false);
          }
        }
      );
      if (cleanedUpRef.current) {
        unlistenComplete();
        unlisteners.forEach(u => u());
        return;
      }
      unlisteners.push(unlistenComplete);

      const unlistenError = await listen<DiarizationError>(
        'diarization-error',
        async (event) => {
          if (event.payload.meeting_id === meetingId) {
            await Analytics.trackError('identify_speakers_failed', event.payload.error);

            setIsProcessing(false);
            setError(event.payload.error);
          }
        }
      );
      if (cleanedUpRef.current) {
        unlistenError();
        unlisteners.forEach(u => u());
        return;
      }
      unlisteners.push(unlistenError);
    };

    setupListeners();

    return () => {
      cleanedUpRef.current = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [meetingId]);

  const handleStart = async () => {
    if (!meetingFolderPath) {
      setError('Meeting folder path not available');
      return;
    }

    setIsProcessing(true);
    setError(null);
    setProgress(null);

    try {
      await Analytics.track('identify_speakers_started', {});
      await invoke('start_speaker_identification_command', {
        meetingId,
        meetingFolderPath,
      });
    } catch (err: any) {
      setIsProcessing(false);
      const errorMsg = typeof err === 'string' ? err : (err?.message || String(err));
      setError(errorMsg);
      await Analytics.trackError('identify_speakers_failed', errorMsg);
    }
  };

  const handleCancel = async () => {
    if (isProcessing) {
      try {
        await invoke('cancel_retranscription_command');
        setIsProcessing(false);
        setProgress(null);
        toast.info('Speaker identification cancelled');
      } catch (err) {
        console.error('Failed to cancel speaker identification:', err);
      }
    }
    onOpenChange(false);
  };

  // Closing while processing does NOT cancel the job - see RetranscribeDialog for
  // the same rationale; use the explicit Cancel button to actually stop it.
  const handleOpenChange = (newOpen: boolean) => {
    onOpenChange(newOpen);
  };

  const handleEscapeKeyDown = (_event: KeyboardEvent) => {
    // No-op: closing (via Escape) while processing is allowed
  };

  const handleInteractOutside = (_event: Event) => {
    // No-op: clicking outside while processing is allowed
  };

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent
        className="sm:max-w-[450px]"
        onEscapeKeyDown={handleEscapeKeyDown}
        onInteractOutside={handleInteractOutside}
      >
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            {isProcessing ? (
              <>
                <Loader2 className="h-5 w-5 animate-spin text-blue-600" />
                Identifying speakers...
              </>
            ) : error ? (
              <>
                <AlertCircle className="h-5 w-5 text-red-600" />
                Speaker Identification Failed
              </>
            ) : (
              <>
                <Users className="h-5 w-5 text-blue-600" />
                Identify Speakers
              </>
            )}
          </DialogTitle>
          <DialogDescription>
            {isProcessing
              ? progress?.message || 'Analyzing system audio...'
              : error
                ? 'An error occurred during speaker identification'
                : 'Analyze this meeting\'s system audio to tell apart individual participants (e.g. "Speaker 1", "Speaker 2") instead of one flat "Others" bucket. Your own microphone audio is already identified and isn\'t affected.'}
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-4 py-4">
          {isProcessing && progress && (
            <div className="space-y-2">
              <div className="relative">
                <div className="w-full bg-gray-200 rounded-full h-3">
                  <div
                    className="bg-blue-600 h-3 rounded-full transition-all duration-300 ease-out"
                    style={{ width: `${Math.min(progress.progress_percentage, 100)}%` }}
                  />
                </div>
                <div className="flex justify-between text-xs text-gray-600 mt-1">
                  <span>{progress.stage}</span>
                  <span>{Math.round(progress.progress_percentage)}%</span>
                </div>
              </div>
              <p className="text-sm text-muted-foreground text-center">
                {progress.message}
              </p>
            </div>
          )}

          {error && (
            <div className="bg-red-50 border border-red-200 rounded-lg p-3">
              <p className="text-sm text-red-800">{error}</p>
            </div>
          )}
        </div>

        <DialogFooter>
          {!isProcessing && !error && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Cancel
              </Button>
              <Button
                onClick={handleStart}
                className="bg-blue-600 hover:bg-blue-700"
                disabled={!meetingFolderPath}
              >
                <Users className="h-4 w-4 mr-2" />
                Start Identifying
              </Button>
            </>
          )}
          {isProcessing && (
            <Button variant="outline" onClick={handleCancel}>
              <X className="h-4 w-4 mr-2" />
              Cancel
            </Button>
          )}
          {error && (
            <>
              <Button variant="outline" onClick={() => onOpenChange(false)}>
                Close
              </Button>
              <Button
                onClick={() => {
                  setError(null);
                  setProgress(null);
                }}
                variant="outline"
              >
                Try Again
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
