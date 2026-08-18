import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranscripts } from '@/contexts/TranscriptContext';
import { useSidebar } from '@/components/Sidebar/SidebarProvider';
import { useConfig } from '@/contexts/ConfigContext';
import { useRecordingState, RecordingStatus } from '@/contexts/RecordingStateContext';
import { recordingService } from '@/services/recordingService';
import Analytics from '@/lib/analytics';
import { showRecordingNotification } from '@/lib/recordingNotification';
import { toast } from 'sonner';

interface UseRecordingStartReturn {
  handleRecordingStart: (transcribeLive: boolean) => Promise<void>;
  isAutoStarting: boolean;
}

/**
 * Custom hook for managing recording start lifecycle.
 * Handles both manual start (button click) and auto-start (from sidebar navigation).
 *
 * Features:
 * - Meeting title generation (format: Meeting DD_MM_YY_HH_MM_SS)
 * - Transcript clearing on start
 * - Analytics tracking
 * - Recording notification display
 * - Auto-start from sidebar via sessionStorage flag
 */
export function useRecordingStart(
  isRecording: boolean,
  setIsRecording: (value: boolean) => void,
  showModal?: (name: 'modelSelector', message?: string) => void
): UseRecordingStartReturn {
  const [isAutoStarting, setIsAutoStarting] = useState(false);

  const { clearTranscripts, setMeetingTitle } = useTranscripts();
  const { setIsMeetingActive } = useSidebar();
  const { selectedDevices } = useConfig();
  const { setStatus } = useRecordingState();

  // Generate meeting title with timestamp
  const generateMeetingTitle = useCallback(() => {
    const now = new Date();
    const day = String(now.getDate()).padStart(2, '0');
    const month = String(now.getMonth() + 1).padStart(2, '0');
    const year = String(now.getFullYear()).slice(-2);
    const hours = String(now.getHours()).padStart(2, '0');
    const minutes = String(now.getMinutes()).padStart(2, '0');
    const seconds = String(now.getSeconds()).padStart(2, '0');
    return `Meeting ${day}_${month}_${year}_${hours}_${minutes}_${seconds}`;
  }, []);

  // Check if Parakeet transcription model is ready
  const checkParakeetReady = useCallback(async (): Promise<boolean> => {
    try {
      await invoke('parakeet_init');
      const hasModels = await invoke<boolean>('parakeet_has_available_models');
      return hasModels;
    } catch (error) {
      console.error('Failed to check Parakeet status:', error);
      return false;
    }
  }, []);

  // Check if any model is currently downloading
  const checkIfModelDownloading = useCallback(async (): Promise<boolean> => {
    try {
      const models = await invoke<any[]>('parakeet_get_available_models');
      const isDownloading = models.some(m =>
        m.status && (
          typeof m.status === 'object'
            ? 'Downloading' in m.status
            : m.status === 'Downloading'
        )
      );
      return isDownloading;
    } catch (error) {
      console.error('Failed to check model download status:', error);
      return false; // Default to not downloading (will show error + modal)
    }
  }, []);

  // Persist the transcribe-live choice made on the record button so it becomes the
  // new default (read server-side by start_recording_with_devices_and_meeting) -
  // reuses the exact same preference the Settings toggle writes, just without a
  // trip through the Settings page. Best-effort: a failure here shouldn't block
  // starting the recording, it just means the choice won't stick as the default.
  const persistTranscribeLivePreference = useCallback(async (transcribeLive: boolean) => {
    try {
      const current = await invoke<Record<string, unknown>>('get_recording_preferences');
      await invoke('set_recording_preferences', {
        preferences: { ...current, transcribe_live: transcribeLive },
      });
    } catch (error) {
      console.warn('Failed to persist transcribe_live preference:', error);
    }
  }, []);

  // Handle manual recording start (from button click)
  // `transcribeLive` is the mode chosen in the record button's dropdown: true =
  // transcribe live (VAD/Whisper alongside capture), false = "record only" (audio
  // saved, transcribe later on demand from meeting-details).
  const handleRecordingStart = useCallback(async (transcribeLive: boolean) => {
    try {
      console.log(`handleRecordingStart called (transcribeLive=${transcribeLive}) - checking model status`);

      // The transcription model readiness check only matters when we're actually
      // going to transcribe - "record only" mode never loads a model during capture.
      if (transcribeLive) {
        const parakeetReady = await checkParakeetReady();
        if (!parakeetReady) {
          const isDownloading = await checkIfModelDownloading();
          if (isDownloading) {
            toast.info('Model download in progress', {
              description: 'Please wait for the transcription model to finish downloading before recording.',
              duration: 5000,
            });
            Analytics.trackButtonClick('start_recording_blocked_downloading', 'home_page');
          } else {
            toast.error('Transcription model not ready', {
              description: 'Please download a transcription model before recording.',
              duration: 5000,
            });
            showModal?.('modelSelector', 'Transcription model setup required');
            Analytics.trackButtonClick('start_recording_blocked_missing', 'home_page');
          }
          setStatus(RecordingStatus.IDLE);
          return;
        }
      }

      console.log('Model check passed - setting up meeting title and state');

      const randomTitle = generateMeetingTitle();
      setMeetingTitle(randomTitle);

      // Persist the chosen mode as the new default before starting, so the backend
      // (which reads this preference server-side) picks it up immediately.
      await persistTranscribeLivePreference(transcribeLive);

      // Set STARTING status before initiating backend recording
      setStatus(RecordingStatus.STARTING, 'Initializing recording...');

      // Start the actual backend recording
      console.log('Starting backend recording with meeting:', randomTitle);
      await recordingService.startRecordingWithDevices(
        selectedDevices?.micDevice || null,
        selectedDevices?.systemDevice || null,
        randomTitle
      );
      console.log('Backend recording started successfully');

      // Update state after successful backend start
      // Note: RECORDING status will be set by RecordingStateContext event listener
      console.log('Setting isRecordingState to true');
      setIsRecording(true); // This will also update the sidebar via the useEffect
      clearTranscripts(); // Clear previous transcripts when starting new recording
      setIsMeetingActive(true);
      Analytics.trackButtonClick('start_recording', 'home_page');

      // Show recording notification if enabled
      await showRecordingNotification();
    } catch (error) {
      console.error('Failed to start recording:', error);
      setStatus(RecordingStatus.ERROR, error instanceof Error ? error.message : 'Failed to start recording');
      setIsRecording(false); // Reset state on error
      Analytics.trackButtonClick('start_recording_error', 'home_page');
      // Re-throw so RecordingControls can handle device-specific errors
      throw error;
    }
  }, [generateMeetingTitle, setMeetingTitle, setIsRecording, clearTranscripts, setIsMeetingActive, checkParakeetReady, checkIfModelDownloading, selectedDevices, showModal, setStatus, persistTranscribeLivePreference]);

  // Listen for direct recording trigger from the system tray ("Start Recording" menu
  // item -> layout.tsx's request-recording-toggle handler forwards here). Unlike the
  // left sidebar button (see SidebarProvider.tsx), the tray can't show a mode-choice
  // dropdown, so it starts immediately using the current default preference.
  useEffect(() => {
    const handleDirectStart = async () => {
      if (isRecording || isAutoStarting) {
        console.log('Recording already in progress, ignoring direct start event');
        return;
      }

      console.log('Direct start from tray - checking Parakeet model status');
      setIsAutoStarting(true);

      // Check if Parakeet transcription model is ready before starting
      const parakeetReady = await checkParakeetReady();
      if (!parakeetReady) {
        const isDownloading = await checkIfModelDownloading();
        if (isDownloading) {
          toast.info('Model download in progress', {
            description: 'Please wait for the transcription model to finish downloading before recording.',
            duration: 5000,
          });
          Analytics.trackButtonClick('start_recording_blocked_downloading', 'sidebar_direct');
        } else {
          toast.error('Transcription model not ready', {
            description: 'Please download a transcription model before recording.',
            duration: 5000,
          });
          showModal?.('modelSelector', 'Transcription model setup required');
          Analytics.trackButtonClick('start_recording_blocked_missing', 'sidebar_direct');
        }
        setStatus(RecordingStatus.IDLE);
        setIsAutoStarting(false);
        return;
      }

      try {
        // Generate meeting title
        const generatedMeetingTitle = generateMeetingTitle();

        // Set STARTING status before initiating backend recording
        setStatus(RecordingStatus.STARTING, 'Initializing recording...');

        console.log('Starting backend recording with meeting:', generatedMeetingTitle);
        const result = await recordingService.startRecordingWithDevices(
          selectedDevices?.micDevice || null,
          selectedDevices?.systemDevice || null,
          generatedMeetingTitle
        );
        console.log('Backend recording result:', result);

        // Update UI state after successful backend start
        // Note: RECORDING status will be set by RecordingStateContext event listener
        setMeetingTitle(generatedMeetingTitle);
        setIsRecording(true);
        clearTranscripts();
        setIsMeetingActive(true);
        Analytics.trackButtonClick('start_recording', 'sidebar_direct');

        // Show recording notification if enabled
        await showRecordingNotification();
      } catch (error) {
        console.error('Failed to start recording from sidebar:', error);
        setStatus(RecordingStatus.ERROR, error instanceof Error ? error.message : 'Failed to start recording from sidebar');
        alert('Failed to start recording. Check console for details.');
        Analytics.trackButtonClick('start_recording_error', 'sidebar_direct');
      } finally {
        setIsAutoStarting(false);
      }
    };

    window.addEventListener('start-recording-from-sidebar', handleDirectStart);

    return () => {
      window.removeEventListener('start-recording-from-sidebar', handleDirectStart);
    };
  }, [
    isRecording,
    isAutoStarting,
    selectedDevices,
    generateMeetingTitle,
    setMeetingTitle,
    setIsRecording,
    clearTranscripts,
    setIsMeetingActive,
    checkParakeetReady,
    checkIfModelDownloading,
    showModal,
    setStatus,
  ]);

  return {
    handleRecordingStart,
    isAutoStarting,
  };
}
