import React, { useState, useEffect, useRef } from 'react';
import { Tag, Loader2, AlertCircle, Check } from 'lucide-react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '../ui/dialog';
import { Button } from '../ui/button';
import { Input } from '../ui/input';
import { invoke } from '@tauri-apps/api/core';
import { toast } from 'sonner';
import Analytics from '@/lib/analytics';
import { Transcript } from '@/types';

interface RenameSpeakersDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  meetingId: string;
  onComplete?: () => void;
}

// One renameable speaker identity found in this meeting's transcript: either the
// "mic" bucket (always "You" by default) or a "system" bucket - either an
// individually-diarized voice (speaker_label set, e.g. "Speaker 1") or the
// generic "Others" catch-all (speaker_label unset). `oldLabel` is exactly what's
// currently stored in speaker_label (null for the un-labeled default buckets) -
// api_rename_speaker needs it to know which rows to update.
interface SpeakerGroup {
  key: string;
  speaker: 'mic' | 'system';
  oldLabel: string | null;
  defaultName: string;
  exampleText: string;
}

// Lets the user replace the generic "You"/"Others"/"Speaker N" badges with real
// names, scoped to this one meeting (see CLAUDE.md's Speaker Attribution section
// for why there's no cross-meeting voice identity yet - this is intentionally the
// simple, per-meeting version of that idea). Renaming just writes into the
// existing speaker_label column, so it reuses VirtualizedTranscriptView's
// SpeakerBadge rendering with no other changes needed.
export function RenameSpeakersDialog({
  open,
  onOpenChange,
  meetingId,
  onComplete,
}: RenameSpeakersDialogProps) {
  const [isLoading, setIsLoading] = useState(false);
  const [isSaving, setIsSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [groups, setGroups] = useState<SpeakerGroup[]>([]);
  const [names, setNames] = useState<Record<string, string>>({});

  const onCompleteRef = useRef(onComplete);
  useEffect(() => { onCompleteRef.current = onComplete; }, [onComplete]);

  useEffect(() => {
    if (!open) return;

    let cancelled = false;
    setIsLoading(true);
    setError(null);

    (async () => {
      try {
        // Fetch every transcript segment for this meeting - the panel's own list
        // may only hold a paginated subset, and a speaker who only appears late
        // in a long meeting would otherwise be missed here.
        const firstPage = await invoke('api_get_meeting_transcripts', {
          meetingId,
          limit: 1,
          offset: 0,
        }) as { total_count: number };

        const totalCount = firstPage.total_count;
        if (totalCount === 0) {
          if (!cancelled) { setGroups([]); setIsLoading(false); }
          return;
        }

        const { transcripts } = await invoke('api_get_meeting_transcripts', {
          meetingId,
          limit: totalCount,
          offset: 0,
        }) as { transcripts: Transcript[] };

        if (cancelled) return;

        // Group by (speaker, speaker_label), preserving first-appearance order so
        // the dialog lists speakers roughly in the order they first talk.
        const seen = new Map<string, SpeakerGroup>();
        for (const t of transcripts) {
          if (!t.speaker) continue; // legacy rows with no source tracked - nothing to rename
          const key = `${t.speaker}:${t.speaker_label ?? ''}`;
          if (seen.has(key)) continue;

          const isMic = t.speaker === 'mic';
          seen.set(key, {
            key,
            speaker: t.speaker,
            oldLabel: t.speaker_label ?? null,
            defaultName: t.speaker_label || (isMic ? 'You' : 'Others'),
            exampleText: t.text?.trim().slice(0, 100) || '',
          });
        }

        const groupList = Array.from(seen.values());
        setGroups(groupList);
        setNames(Object.fromEntries(groupList.map(g => [g.key, g.defaultName])));
      } catch (err: any) {
        if (!cancelled) {
          setError(typeof err === 'string' ? err : (err?.message || String(err)));
        }
      } finally {
        if (!cancelled) setIsLoading(false);
      }
    })();

    return () => { cancelled = true; };
  }, [open, meetingId]);

  const handleSave = async () => {
    // Only touch groups whose name actually changed - avoids pointless writes and
    // keeps rows that already had a diarized "Speaker N" label untouched unless
    // the user explicitly renamed them.
    const changed = groups.filter(g => names[g.key]?.trim() && names[g.key].trim() !== g.defaultName);

    if (changed.length === 0) {
      onOpenChange(false);
      return;
    }

    setIsSaving(true);
    setError(null);

    try {
      for (const g of changed) {
        await invoke('api_rename_speaker', {
          meetingId,
          speaker: g.speaker,
          oldLabel: g.oldLabel,
          newLabel: names[g.key].trim(),
        });
      }

      await Analytics.track('rename_speakers_saved', { count: changed.length.toString() });
      toast.success(`Renamed ${changed.length} speaker${changed.length === 1 ? '' : 's'}.`);
      onCompleteRef.current?.();
      onOpenChange(false);
    } catch (err: any) {
      const errorMsg = typeof err === 'string' ? err : (err?.message || String(err));
      setError(errorMsg);
      await Analytics.trackError('rename_speakers_failed', errorMsg);
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-[480px]">
        <DialogHeader>
          <DialogTitle className="flex items-center gap-2">
            <Tag className="h-5 w-5 text-blue-600" />
            Rename Speakers
          </DialogTitle>
          <DialogDescription>
            Replace the generic labels below with real names for this meeting. This only
            applies here - the same person will show up as a new "Speaker N" in other meetings.
          </DialogDescription>
        </DialogHeader>

        <div className="space-y-3 py-2 max-h-[50vh] overflow-y-auto">
          {isLoading && (
            <div className="flex items-center justify-center py-8 text-muted-foreground">
              <Loader2 className="h-5 w-5 animate-spin mr-2" />
              Loading speakers...
            </div>
          )}

          {!isLoading && !error && groups.length === 0 && (
            <p className="text-sm text-muted-foreground text-center py-8">
              No speakers found in this meeting's transcript yet.
            </p>
          )}

          {!isLoading && groups.map((g) => (
            <div key={g.key} className="space-y-1">
              <Input
                value={names[g.key] ?? ''}
                onChange={(e) => setNames(prev => ({ ...prev, [g.key]: e.target.value }))}
                placeholder={g.defaultName}
                disabled={isSaving}
              />
              {g.exampleText && (
                <p className="text-xs text-muted-foreground pl-1 truncate">
                  "{g.exampleText}{g.exampleText.length >= 100 ? '…' : ''}"
                </p>
              )}
            </div>
          ))}

          {error && (
            <div className="bg-red-50 border border-red-200 rounded-lg p-3 flex items-start gap-2">
              <AlertCircle className="h-4 w-4 text-red-600 mt-0.5 flex-shrink-0" />
              <p className="text-sm text-red-800">{error}</p>
            </div>
          )}
        </div>

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)} disabled={isSaving}>
            Cancel
          </Button>
          <Button
            onClick={handleSave}
            className="bg-blue-600 hover:bg-blue-700"
            disabled={isLoading || isSaving || groups.length === 0}
          >
            {isSaving ? (
              <Loader2 className="h-4 w-4 mr-2 animate-spin" />
            ) : (
              <Check className="h-4 w-4 mr-2" />
            )}
            Save
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
