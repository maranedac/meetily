"use client";

import { useState, useCallback } from 'react';
import { Button } from '@/components/ui/button';
import { ButtonGroup } from '@/components/ui/button-group';
import { Copy, FolderOpen, RefreshCw, Mic, Users } from 'lucide-react';
import Analytics from '@/lib/analytics';
import { RetranscribeDialog } from './RetranscribeDialog';
import { DiarizeDialog } from './DiarizeDialog';


interface TranscriptButtonGroupProps {
  transcriptCount: number;
  onCopyTranscript: () => void;
  onOpenMeetingFolder: () => Promise<void>;
  meetingId?: string;
  meetingFolderPath?: string | null;
  onRefetchTranscripts?: () => Promise<void>;
  // 'pending' = meeting was recorded audio-only ("record only" mode) and has no
  // transcript yet - offers the "Transcribe" action below regardless of the beta flag.
  transcriptionStatus?: string;
}


export function TranscriptButtonGroup({
  transcriptCount,
  onCopyTranscript,
  onOpenMeetingFolder,
  meetingId,
  meetingFolderPath,
  onRefetchTranscripts,
  transcriptionStatus,
}: TranscriptButtonGroupProps) {
  const [showRetranscribeDialog, setShowRetranscribeDialog] = useState(false);
  const [showTranscribeDialog, setShowTranscribeDialog] = useState(false);
  const [showDiarizeDialog, setShowDiarizeDialog] = useState(false);
  const isPendingTranscription = transcriptionStatus === 'pending';

  const handleRetranscribeComplete = useCallback(async () => {
    // Refetch transcripts to show the updated data
    if (onRefetchTranscripts) {
      await onRefetchTranscripts();
    }
  }, [onRefetchTranscripts]);

  return (
    <div className="flex items-center justify-center w-full gap-2">
      <ButtonGroup>
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            Analytics.trackButtonClick('copy_transcript', 'meeting_details');
            onCopyTranscript();
          }}
          disabled={transcriptCount === 0}
          title={transcriptCount === 0 ? 'No transcript available' : 'Copy Transcript'}
        >
          <Copy />
          <span className="hidden lg:inline">Copy</span>
        </Button>

        <Button
          size="sm"
          variant="outline"
          className="xl:px-4"
          onClick={() => {
            Analytics.trackButtonClick('open_recording_folder', 'meeting_details');
            onOpenMeetingFolder();
          }}
          title="Open Recording Folder"
        >
          <FolderOpen className="xl:mr-2" size={18} />
          <span className="hidden lg:inline">Recording</span>
        </Button>

        {isPendingTranscription && meetingId && meetingFolderPath && (
          <Button
            size="sm"
            variant="outline"
            className="bg-gradient-to-r from-blue-50 to-purple-50 hover:from-blue-100 hover:to-purple-100 border-blue-200 xl:px-4"
            onClick={() => {
              Analytics.trackButtonClick('transcribe_recording', 'meeting_details');
              setShowTranscribeDialog(true);
            }}
            title="Transcribe this audio-only recording"
          >
            <Mic className="xl:mr-2" size={18} />
            <span className="hidden lg:inline">Transcribe</span>
          </Button>
        )}

        {!isPendingTranscription && meetingId && meetingFolderPath && (
          <Button
            size="sm"
            variant="outline"
            className="bg-gradient-to-r from-blue-50 to-purple-50 hover:from-blue-100 hover:to-purple-100 border-blue-200 xl:px-4"
            onClick={() => {
              Analytics.trackButtonClick('enhance_transcript', 'meeting_details');
              setShowRetranscribeDialog(true);
            }}
            title="Re-transcribe with a different model or language"
          >
            <RefreshCw className="xl:mr-2" size={18} />
            <span className="hidden lg:inline">Enhance</span>
          </Button>
        )}

        {!isPendingTranscription && meetingId && meetingFolderPath && transcriptCount > 0 && (
          <Button
            size="sm"
            variant="outline"
            className="bg-gradient-to-r from-blue-50 to-purple-50 hover:from-blue-100 hover:to-purple-100 border-blue-200 xl:px-4"
            onClick={() => {
              Analytics.trackButtonClick('identify_speakers', 'meeting_details');
              setShowDiarizeDialog(true);
            }}
            title="Identify individual speakers in the system audio"
          >
            <Users className="xl:mr-2" size={18} />
            <span className="hidden lg:inline">Speakers</span>
          </Button>
        )}
      </ButtonGroup>

      {isPendingTranscription && meetingId && meetingFolderPath && (
        <RetranscribeDialog
          open={showTranscribeDialog}
          onOpenChange={setShowTranscribeDialog}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onComplete={handleRetranscribeComplete}
          mode="transcribe"
        />
      )}

      {!isPendingTranscription && meetingId && meetingFolderPath && (
        <RetranscribeDialog
          open={showRetranscribeDialog}
          onOpenChange={setShowRetranscribeDialog}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onComplete={handleRetranscribeComplete}
        />
      )}

      {!isPendingTranscription && meetingId && meetingFolderPath && transcriptCount > 0 && (
        <DiarizeDialog
          open={showDiarizeDialog}
          onOpenChange={setShowDiarizeDialog}
          meetingId={meetingId}
          meetingFolderPath={meetingFolderPath}
          onComplete={handleRetranscribeComplete}
        />
      )}
    </div>
  );
}
