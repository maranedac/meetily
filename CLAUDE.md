# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Meetily** is a privacy-first AI meeting assistant that captures, transcribes, and summarizes meetings entirely on local infrastructure. The supported application is the Tauri desktop app with a Rust core.

1. **Frontend**: Tauri-based desktop application (Rust + Next.js + TypeScript)
2. **Rust Backend**: Tauri commands, audio capture, transcription, storage, and summarization orchestration
3. **Legacy Backend Archive**: the old Python/FastAPI, Docker, and standalone whisper-server backend under `backend/` is archived and unsupported

### Key Technology Stack
- **Desktop App**: Tauri 2.x (Rust) + Next.js 14 + React 18
- **Audio Processing**: Rust (cpal, whisper-rs, professional audio mixing)
- **Transcription**: Whisper.cpp / whisper-rs and Parakeet paths in the Tauri app
- **App API Surface**: Tauri commands and events, not a separate FastAPI service
- **LLM Integration**: Ollama (local), Claude, Groq, OpenRouter

## Essential Development Commands

### Frontend Development (Tauri Desktop App)

**Location**: `/frontend`

```bash
# macOS Development
./clean_run.sh              # Clean build and run with info logging
./clean_run.sh debug        # Run with debug logging
./clean_build.sh            # Production build

# Windows Development
clean_run_windows.bat       # Clean build and run
clean_build_windows.bat     # Production build

# Manual Commands
pnpm install                # Install dependencies
pnpm run dev                # Next.js dev server (port 3118)
pnpm run tauri:dev          # Full Tauri development mode
pnpm run tauri:build        # Production build

# GPU-Specific Builds (for testing acceleration)
pnpm run tauri:dev:metal    # macOS Metal GPU
pnpm run tauri:dev:cuda     # NVIDIA CUDA
pnpm run tauri:dev:vulkan   # AMD/Intel Vulkan
pnpm run tauri:dev:cpu      # CPU-only (no GPU)
```

### Legacy Backend Archive

**Location**: `/backend`

The Python/FastAPI backend, Docker setup, and standalone whisper-server scripts are archived for historical reference and migration context only. Do not use them for current development, new installs, production deployments, or issue triage for the supported app.

The archived FastAPI service had unauthenticated, development-oriented CORS behavior. Treat that behavior as obsolete legacy context, not as a supported production API.

### Service Endpoints
- **Frontend Dev**: http://localhost:3118

## High-Level Architecture

### Tauri Desktop Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Frontend (Tauri Desktop App)                  │
│  ┌──────────────────┐  ┌─────────────────┐  ┌────────────────┐ │
│  │   Next.js UI     │  │  Rust Backend   │  │ Whisper Engine │ │
│  │  (React/TS)      │←→│  (Audio + IPC)  │←→│  (Local STT)   │ │
│  └──────────────────┘  └─────────────────┘  └────────────────┘ │
│         ↑ Tauri Events           ↑ Audio Pipeline               │
└─────────────────────────────────────────────────────────────────┘
```

The current app does not require a separate FastAPI tier. Meeting persistence, local transcription, and summary orchestration are handled through the Rust/Tauri core.

### Audio Processing Pipeline (Critical Understanding)

The audio system has **two parallel paths** with different purposes:

```
Raw Audio (Mic + System, captured as separate streams)
         ↓
┌────────────────────────────────────────────────────────────┐
│              Audio Pipeline Manager                         │
│  (frontend/src-tauri/src/audio/pipeline.rs)                │
└─────────────┬──────────────────────────┬───────────────────┘
              ↓                          ↓
    ┌─────────────────┐        ┌──────────────────────────────┐
    │ Recording Path  │        │ Transcription Path            │
    │ (Mixed, for the │        │ (VAD-filtered, PER SOURCE)    │
    │  saved audio    │        │ mic window → VAD → Whisper    │
    │  file only)     │        │ sys window → VAD → Whisper    │
    └─────────────────┘        │ (independent VAD processors)  │
              ↓                └──────────────────────────────┘
    RecordingSaver.save()                  ↓
                                WhisperEngine.transcribe()
                                (each segment tagged "mic"/"system")
```

**Key Insight**: The pipeline performs **professional audio mixing** (RMS-based ducking, clipping prevention) only for the saved recording file. For transcription, mic and system audio are run through **two independent VAD processors** and transcribed **separately** (not pre-mixed) so each transcript segment can be attributed to its real source. See "Speaker Attribution" below.

### Audio Device Modularization (Recently Completed)

**Context**: The audio system was refactored from a monolithic 1028-line `core.rs` file into focused modules. See [AUDIO_MODULARIZATION_PLAN.md](AUDIO_MODULARIZATION_PLAN.md) for details.

```
audio/
├── devices/                    # Device discovery and configuration
│   ├── discovery.rs           # list_audio_devices, trigger_audio_permission
│   ├── microphone.rs          # default_input_device
│   ├── speakers.rs            # default_output_device
│   ├── configuration.rs       # AudioDevice types, parsing
│   └── platform/              # Platform-specific implementations
│       ├── windows.rs         # WASAPI logic (~200 lines)
│       ├── macos.rs           # ScreenCaptureKit logic
│       └── linux.rs           # ALSA/PulseAudio logic
├── capture/                   # Audio stream capture
│   ├── microphone.rs          # Microphone capture stream
│   ├── system.rs              # System audio capture stream
│   └── core_audio.rs          # macOS ScreenCaptureKit integration
├── pipeline.rs                # Audio mixing and VAD processing
├── recording_manager.rs       # High-level recording coordination
├── recording_commands.rs      # Tauri command interface
└── recording_saver.rs         # Audio file writing
```

**When working on audio features**:
- Device detection issues → `devices/discovery.rs` or `devices/platform/{windows,macos,linux}.rs`
- Microphone/speaker problems → `devices/microphone.rs` or `devices/speakers.rs`
- Audio capture issues → `capture/microphone.rs` or `capture/system.rs`
- Mixing/processing problems → `pipeline.rs`
- Recording workflow → `recording_manager.rs`

### Speaker Attribution: "mic" vs "system" (Phase 1 — implemented)

**Goal**: label each transcript segment with who said it — `"mic"` (you) or `"system"` (everyone else, via loopback/system audio) — as a stepping stone toward full speaker diarization (Phase 2 — implemented for offline/"record only" meetings, see below; Phase 3, live diarization, not started).

**Why this works without acoustic diarization**: mic and system audio are captured as genuinely separate hardware streams. Instead of mixing them before Whisper (which destroys the source information), each stream now gets its own `ContinuousVadProcessor` and is transcribed independently, so the origin is known for free.

**Data flow**:
```
pipeline.rs: AudioPipeline::run()
  → extract_window() returns (mic_window, sys_window) separately
  → mixer.mix_window(...) still runs, but ONLY feeds the recorded WAV file
  → process_window_for_source(&mic_window, DeviceType::Microphone)
  → process_window_for_source(&sys_window, DeviceType::System)
      (each calls its OWN VadProcessor: self.vad_processor vs self.vad_processor_system)
  → transcription/worker.rs: device_type_to_source() maps DeviceType → "mic"/"system"
  → TranscriptUpdate.source field carries it to the frontend via the "transcript-update" event
```

**Persistence** — the `speaker` column already existed in the `transcripts` SQLite table from an old, never-wired-up migration (`20251110000001_add_speaker_field.sql`); Phase 1 connected it end-to-end. There are **three** separate "transcript segment" struct families in Rust — when adding a field like this, all of them (and every place that converts between them) need updating, or it silently gets dropped:
1. `api::api::TranscriptSegment` — used by `api_save_transcript` (the real DB write path, from the frontend's live buffer).
2. `audio::recording_saver::TranscriptSegment` — used by `RECORDING_MANAGER` (in-memory history + incremental `transcripts.json` backup; fed by a **Rust-side** `app.listen("transcript-update", ...)` in `recording_commands.rs`, not just the frontend listener).
3. `api::api::MeetingTranscript` — the response DTO for `api_get_meeting_transcripts` / `api_get_meeting`; built by *converting* the DB-read `database::models::Transcript` in two places (`api/api.rs` and `database/repositories/meeting.rs`). This conversion step is the easiest place to silently drop a new field, since it doesn't error at compile time if you just forget one line in the `map()` closure.

Frontend: `speaker?: 'mic' | 'system'` on `Transcript`, `TranscriptUpdate`, and `TranscriptSegmentData` (`src/types/index.ts`). Rendered as a small "You"/"Others" badge in `VirtualizedTranscriptView.tsx`.

**Known limitation**: if the mic can acoustically/electrically pick up the system audio (no headphones, or a cheap headset where the driver leaks into an inline mic), the same speech can get transcribed on *both* streams, showing up duplicated under "You". This was observed once and did not reproduce on a second recording with identical settings — suspected transient buffer-sync skew between the two independent ring buffers right at recording start, not confirmed. If it recurs consistently, consider a text-similarity de-dup heuristic between concurrent mic/system segments, or real acoustic echo cancellation (AEC) using the system stream as the reference signal.

**Phase 2 (implemented, offline-only)**: real automatic speaker diarization — identifies each individual "other" participant separately within the "system" bucket (e.g. "Speaker 1", "Speaker 2"), instead of one flat "Others". Runs **only on the system-audio stream** (mic is already solved "for free" — always you) and **only for "record only" meetings** (see next section) that have a separate `system.mp4` track to analyze; live-transcribed or imported single-file meetings have no isolated system audio to diarize after the fact.

- Model: **WeSpeaker ResNet34** (English, VoxCeleb-trained, ~25MB ONNX), downloaded on demand from the `k2-fsa/sherpa-onnx` GitHub release `speaker-recognition-models` (Apache-2.0, no login/gate). Deliberately **not** `pyannote/embedding` — also fine license-wise (MIT) but gated behind a HuggingFace click-through agreement, a bad fit for silent auto-download like Whisper/Parakeet already do. Dead code in `audio/stt.rs` (not compiled — not in any `mod` declaration) references `pyannote`/`speaker_embedding` from an old "screenpipe" ancestor project — not reusable (references modules that no longer exist in this codebase), only useful as a conceptual pointer.
- Feature extraction: the embedding model expects Kaldi-style **fbank features**, not raw waveform (unlike Parakeet, which bundles its own preprocessor ONNX) — computed via the `kaldi-native-fbank` crate (MIT, pure-Rust port of the C++ library sherpa-onnx itself uses, so numerically compatible). **Gotcha already hit**: don't hardcode the feature dimension — `kaldi_native_fbank`'s default options can add extras (e.g. an energy channel) beyond the requested `num_bins`, so the actual per-frame width must be read from the first computed frame, not assumed to equal `num_bins`. Also set `use_energy = false` explicitly since the model expects exactly `num_bins` (80) channels.
- Model I/O: input/output tensor **names** are read dynamically from the loaded `ort::Session` (`session.inputs[0].name` / `session.outputs[0].name`) rather than hardcoded — the actual names turned out to be `"feats"`/`"embs"`, but relying on that being stable across model updates would be fragile.
- Clustering: plain agglomerative clustering (cosine distance, average linkage) implemented by hand in `diarization_engine/cluster.rs` — no new dependency, since a single meeting's segment count is small enough that O(n²) is trivial. Merge threshold is `0.25` cosine distance — **not empirically calibrated yet**, just a starting point; revisit if speakers are consistently over- or under-split. Only assigns "Speaker N" labels when clustering finds 2+ distinct speakers — a single detected voice intentionally stays "Others" (no value in a redundant "Speaker 1").
- Runs automatically as the last step of `retranscription.rs::run_retranscription()`, best-effort — never fails the transcription itself if the model isn't downloaded or inference errors out (logs a `warn!` and leaves `speaker_label` unset).
- New DB column: `transcripts.speaker_label` (nullable `TEXT`, migration `20260807000000_add_speaker_label.sql`) — **additive**, does not touch the existing `speaker` column (still just `"mic"`/`"system"`, unchanged everywhere). Frontend: `speaker_label?: string` alongside `speaker?: 'mic'|'system'` in `types/index.ts`; `VirtualizedTranscriptView.tsx`'s `SpeakerBadge` shows `speaker_label` when present, else falls back to the old binary "You"/"Others".
- Settings UI: `components/DiarizationModelManager.tsx` (Settings → Transcription → "Speaker Identification" section) — manual download only, mirrors the Parakeet model-manager pattern but much simpler (one file, no resume-download support needed at ~25MB).
- **Phase 3, part A (implemented)**: extending diarization to live-transcribed (not just "record only") meetings, without re-transcribing. Two pieces:
  - `recording_saver.rs::start_accumulation()` now also persists a raw, unmixed `system.mp4` track when `transcribe_live` is true (previously this only happened in "record only" mode). The pipeline forwarding (`pipeline.rs` STEP 5) was already sender-presence-based rather than `transcribe_live`-gated, so no pipeline changes were needed — only the saver-creation gate.
  - **Update (2026-08-07)**: `mic.mp4` is now *also* persisted in live-transcribed mode (previously "intentionally not duplicated... live attribution already tagged it 'mic' in real time, so there's nothing to gain"). That assumption broke once "Enhance" offline re-transcription started being used on live-transcribed meetings: `run_retranscription()`'s `find_dual_audio_files()` requires *both* `mic.*` and `system.*`, so without a raw mic track it silently fell back to the single mixed `audio.mp4` with `speaker: None` on every segment — discarding the mic/system attribution the live pass had already established, observed in practice as "Enhance" making an already-decent live transcript worse (no "You"/"Others"/"Speaker N" at all). `start_accumulation()`'s branching on `transcribe_live` for raw-track creation was removed — both tracks are now saved whenever `auto_save` is true, regardless of mode. `transcribe_live` is still passed through for logging only. Meetings recorded *before* this change have no `mic.mp4` and still hit the old fallback if re-enhanced; use "Identify speakers" (`system.*`-only) for those instead.
  - New standalone action "Identify speakers" (`TranscriptButtonGroup.tsx` → `DiarizeDialog.tsx` → `start_speaker_identification_command` → `retranscription.rs::run_speaker_identification()`): loads a meeting's **existing** transcript rows straight from the DB (via the same `MeetingsRepository::get_meeting_transcripts_paginated` used elsewhere, unlimited page size), finds its `system.*` track via a new `find_system_audio_file()` (factored out of `find_dual_audio_files()`'s per-extension lookup, now a shared `find_track_file(folder, base_name)` helper), and calls the same `run_diarization()` full retranscription already used — it was already written source-agnostic (just needs `speaker`/timestamp-tagged segments + a system-track path), so no diarization engine changes were needed. Results are written back with `UPDATE transcripts SET speaker_label = ...` (every "system" row, not just changed ones, so a re-run after recalibrating the clustering threshold correctly clears stale labels too) plus a `transcripts.json` rewrite. Reuses the existing `RETRANSCRIPTION_IN_PROGRESS`/`RETRANSCRIPTION_CANCELLED` guard and `cancel_retranscription_command` rather than adding a second job-tracking mechanism — the app already only supports one background transcription-ish job at a time. `run_diarization()` itself was changed to return `Result<usize>` (distinct speaker count) instead of `Result<()>`, and gained a per-segment cancellation check + a few `emit_progress` calls, both used by the new standalone path (the original `run_retranscription()` call site is otherwise unaffected).
  - **Phase 3, part B (still not started)**: true live/real-time diarization *during* recording (incremental clustering as speech arrives, re-labeling already-shown segments as clusters get refined). Meaningfully harder than part A — no code written toward it yet.

### "Record Only" Mode (implemented)

**Why**: live transcription (VAD + Whisper/Parakeet running alongside audio capture) competes with capture for CPU, and on a slow model/long recording the serial transcription worker (see below) can fall behind real time. Since many users mostly care about the final summary and rarely watch the live transcript, recording and transcribing were decoupled into two explicit choices.

- Preference: `RecordingPreferences.transcribe_live: bool` (default `true`), persisted like `auto_save`. Read server-side in `recording_commands.rs` at recording start — no new frontend invoke param needed.
- UI: the central record button (`RecordingControls.tsx`) is a dropdown with two explicit choices ("Record & transcribe" / "Record only"), calling `handleStartRecording(transcribeLive: boolean)` (`useRecordingStart.ts`), which persists the choice as the new default before starting. The left-sidebar "Start Recording" button and tray quick-action just navigate/start with the current default (a native tray menu can't show a dropdown) — see `SidebarProvider.tsx::handleRecordingToggle` and `useRecordingStart.ts`'s tray-triggered listener.
- When off: `AudioPipeline::run()` (`pipeline.rs`) skips `process_window_for_source()` entirely (no VAD, no Whisper/Parakeet) — that's where the CPU savings come from. `recording_commands.rs` never starts the transcription task.
- Still saves **raw, unmixed** `mic.mp4` and `system.mp4` tracks (in addition to the usual mixed `audio.mp4` used for playback) via a generalized `IncrementalAudioSaver` (now takes a `base_name` param — `"audio"` keeps the original `.checkpoints/` dir name relied on by crash-recovery commands, other names get their own `.checkpoints_{base_name}/`). **As of 2026-08-07 this is no longer exclusive to "record only" mode** — live-transcribed meetings now persist the same two raw tracks (see the Phase 3A update note above), so this is really just describing `start_accumulation()`'s general `auto_save` behavior, not something specific to record-only. These two extra files are what enables offline retranscription ("Enhance") to still separate mic vs system regardless of how the meeting was recorded (and, if downloaded, run diarization on `system.mp4`).
- New `meetings.transcription_status` column (`'pending'`|`'completed'`, migration `20260806000000_add_transcription_status.sql`) tracks whether a "record only" meeting still needs transcribing. **Gotcha already hit**: adding this column required updating explicit-column `SELECT`s in **three** places, not the two obvious repository functions — `meeting.rs`'s `get_meeting`/`get_meeting_metadata` *and* `api.rs::open_meeting_folder` (a third, easy-to-miss query in a different file). `SELECT * FROM meetings` call sites pick up new columns automatically via `MeetingModel`'s `FromRow`; every explicit-column one needs the column added by hand — same class of gotcha as the `speaker` field one above, worth grepping `FROM meetings` broadly whenever this schema changes again.

### Offline Retranscription (implemented)

`audio/retranscription.rs::run_retranscription()` — decode → VAD → transcribe → save, run against already-recorded audio instead of a live stream. Two entry points in the UI, same underlying command (`start_retranscription_command`) and dialog (`RetranscribeDialog.tsx`, distinguished only by a `mode: 'transcribe' | 'enhance'` prop for copy/labeling):
- **"Transcribe"** (`TranscriptButtonGroup.tsx`, always visible — not beta-gated): for meetings with `transcription_status === 'pending'` (i.e. "record only" mode meetings with no transcript yet).
- **"Enhance"** (same component, gated behind `betaFeatures.importAndRetranscribe`): re-transcribe an *already-transcribed* meeting with a different model/language. Already replaces rather than accumulates (`DELETE FROM transcripts` then re-`INSERT` in one transaction) and keeps the audio file, so it can be retried again with yet another model.

`find_dual_audio_files()` auto-detects whether `mic.*`+`system.*` exist and, if so, runs `transcribe_audio_file()` **twice** (once per track, extracted as a shared helper) tagging segments with speaker and merging chronologically by `audio_start_time` before saving — otherwise falls back to the original single-mixed-file path (`find_audio_file()`, `speaker: None`), so old/imported meetings (or any meeting recorded before the 2026-08-07 change described under Phase 3A above, which lacks a `mic.*` file) are unaffected/still degrade gracefully to no attribution rather than erroring. The command backing "Enhance"/"Transcribe" runs fire-and-forget on the Rust side (`tauri::async_runtime::spawn`, returns immediately) — the frontend dialog can be closed while it keeps running in the background; its progress/completion event listeners are intentionally *not* gated on the dialog's `open` state so the completion toast still fires even after closing.

**Gotcha already hit and fixed**: `vad.rs`'s `ContinuousVadProcessor` had a pre-existing timestamp bug (not introduced by retranscription, but only visible there) — on `VadTransition::SpeechStart`, `speech_start_sample` was computed as `processed_samples + timestamp_ms_as_samples`, double-counting an offset (the transition's `timestamp_ms` is already absolute, matching how `SpeechEnd`'s `start_timestamp_ms`/`end_timestamp_ms` are used directly with no offset a few lines below). Invisible in the live pipeline (recording rarely stops mid-utterance in a way that hits `flush()`'s force-end path with a large `processed_samples`), but reliably wrong for the *last* segment of any offline file that happens to end mid-speech — start time could land seconds/minutes past the true end, producing a negative duration.

### Windows Dev Loop Flakiness (transient linker issue)

Occasionally, `pnpm run tauri:dev` fails the final link step with `STATUS_DLL_INIT_FAILED` (`0xc0000142`) even though the exact same code compiles and links fine via a plain `cargo build`. Root cause not fully confirmed (suspected MSVC linker/`mspdbsrv.exe` contention specific to how `tauri-cli` orchestrates the build, not a code issue) — workaround that has reliably worked: run `cargo build` (in `frontend/src-tauri`) to completion first, then immediately run `pnpm run tauri:dev` right after. Also: killing a `pnpm run tauri:dev` process tree can leave the `next dev -p 3118` child orphaned, holding the port — if the next launch fails with `EADDRINUSE`, find and kill the orphaned `node.exe` processes before retrying.

### Rust ↔ Frontend Communication (Tauri Architecture)

**Command Pattern** (Frontend → Rust):
```typescript
// Frontend: src/app/page.tsx
await invoke('start_recording', {
  mic_device_name: "Built-in Microphone",
  system_device_name: "BlackHole 2ch",
  meeting_name: "Team Standup"
});
```

```rust
// Rust: src/lib.rs
#[tauri::command]
async fn start_recording<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    meeting_name: Option<String>
) -> Result<(), String> {
    // Implementation delegates to audio::recording_commands
}
```

**Event Pattern** (Rust → Frontend):
```rust
// Rust: Emit transcript updates
app.emit("transcript-update", TranscriptUpdate {
    text: "Hello world".to_string(),
    timestamp: chrono::Utc::now(),
    // ...
})?;
```

```typescript
// Frontend: Listen for events
await listen<TranscriptUpdate>('transcript-update', (event) => {
  setTranscripts(prev => [...prev, event.payload]);
});
```

### Whisper Model Management

**Model Storage Locations**:
- **Development**: `frontend/models/`
- **Production (macOS)**: `~/Library/Application Support/Meetily/models/`
- **Production (Windows)**: `%APPDATA%\Meetily\models\`

**Model Loading** (frontend/src-tauri/src/whisper_engine/whisper_engine.rs):
```rust
pub async fn load_model(&self, model_name: &str) -> Result<()> {
    // Automatically detects GPU capabilities (Metal/CUDA/Vulkan)
    // Falls back to CPU if GPU unavailable
}
```

**GPU Acceleration**:
- **macOS**: Metal + CoreML (automatically enabled)
- **Windows/Linux**: CUDA (NVIDIA), Vulkan (AMD/Intel), or CPU
- Configure via Cargo features: `--features cuda`, `--features vulkan`

### Summary Templates (view/create/edit/delete — implemented)

Templates control the sections + LLM instructions used when generating a meeting summary (`summary/templates/` on the Rust side). Three tiers, checked in this order at read time (`templates::get_template`): custom (user data dir) → bundled (app resources) → built-in (compiled into the binary via `include_str!` in `templates/defaults.rs`, currently only `daily_standup` and `standard_meeting` — the other JSON files in `frontend/src-tauri/templates/` are NOT wired into the binary and only surface if present in the bundled resources dir).

**Commands**: `api_list_templates`, `api_get_template_details` (summary only), `api_get_template_full` (full sections, for editing), `api_save_template` (create/update — writes to the custom dir; auto-generates a collision-free id from the name if none given), `api_delete_template` (only allowed on custom templates — built-in/bundled are read-only).

**UI**: `src/components/TemplateManager/TemplateManager.tsx` — shared component used both as a dialog (via "Manage templates..." in the Template dropdown on the meeting details summary panel) and inline (Settings → Templates tab). Built-in/bundled templates show a 🔒 read-only lock; only custom templates get edit/delete.

**Gotcha — the `format` field (`paragraph`/`list`/`string`) on a template section is validated but currently has NO effect on generation.** Only `title`, `instruction`, and `item_format` actually reach the LLM prompt (`Template::to_section_instructions()` in `templates/types.rs`). If asked to make `format` do something, it needs to be wired into that function — right now it's dead weight, kept only for schema/UI consistency.

### Summary Generation Timing (implemented)

`useSummaryGeneration.ts` tracks how long a generation/regeneration takes (`generationStartRef`, set when the request fires) and shows it once complete: the success toast description becomes `"Completed in 1m 12s"`, and the status badge on the summary panel becomes `"Summary completed in 1m 12s"` (via `getSummaryStatusMessage`). Purely a frontend timer around the existing polling flow — no backend timing was added.

## Critical Development Patterns

### 1. Audio Buffer Management

**Ring Buffer Mixing** (pipeline.rs):
- Mic and system audio arrive asynchronously at different rates
- Ring buffer accumulates samples until both streams have aligned windows (50ms)
- Professional mixing applies RMS-based ducking to prevent system audio from drowning out microphone
- Uses `VecDeque` for efficient windowed processing

### 2. Thread Safety and Async Boundaries

**Recording State** (recording_state.rs):
```rust
pub struct RecordingState {
    is_recording: Arc<AtomicBool>,
    audio_sender: Arc<RwLock<Option<mpsc::UnboundedSender<AudioChunk>>>>,
    // ...
}
```

**Key Pattern**: Use `Arc<RwLock<T>>` for shared state across async tasks, `Arc<AtomicBool>` for simple flags.

### 3. Error Handling and Logging

**Performance-Aware Logging** (lib.rs):
```rust
#[cfg(debug_assertions)]
macro_rules! perf_debug {
    ($($arg:tt)*) => { log::debug!($($arg)*) };
}

#[cfg(not(debug_assertions))]
macro_rules! perf_debug {
    ($($arg:tt)*) => {};  // Zero overhead in release builds
}
```

**Usage**: Use `perf_debug!()` and `perf_trace!()` for hot-path logging that should be eliminated in production.

### 4. Frontend State Management

**Sidebar Context** (components/Sidebar/SidebarProvider.tsx):
- Global state for meetings list, current meeting, recording status
- Communicates with the Rust/Tauri core through Tauri commands and events
- Keeps React state synchronized with native recording, meeting, transcript, and summary state

**Pattern**: Tauri commands update Rust state → Emit events → Frontend listeners update React state → Context propagates to components

## Common Development Tasks

### Adding a New Audio Device Platform

1. Create platform file: `audio/devices/platform/{platform_name}.rs`
2. Implement device enumeration for the platform
3. Add platform-specific configuration in `audio/devices/configuration.rs`
4. Update `audio/devices/platform/mod.rs` to export new platform functions
5. Test with `cargo check` and platform-specific device tests

### Adding a New Tauri Command

1. Define command in `src/lib.rs`:
   ```rust
   #[tauri::command]
   async fn my_command(arg: String) -> Result<String, String> { /* ... */ }
   ```
2. Register in `tauri::Builder`:
   ```rust
   .invoke_handler(tauri::generate_handler![
       start_recording,
       my_command,  // Add here
   ])
   ```
3. Call from frontend:
   ```typescript
   const result = await invoke<string>('my_command', { arg: 'value' });
   ```

### Modifying Audio Pipeline Behavior

**Location**: `frontend/src-tauri/src/audio/pipeline.rs`

Key components:
- `AudioMixerRingBuffer`: Manages mic + system audio synchronization
- `ProfessionalAudioMixer`: RMS-based ducking and mixing
- `AudioPipelineManager`: Orchestrates VAD, mixing, and distribution

**Testing Audio Changes**:
```bash
# Enable verbose audio logging
RUST_LOG=app_lib::audio=debug ./clean_run.sh

# Monitor audio metrics in real-time
# Check Developer Console in the app (Cmd+Shift+I on macOS)
```

### Tauri Backend Development

Current app behavior should be implemented in the Rust/Tauri core, not in the archived Python backend. Add new frontend-facing behavior through Tauri commands/events and existing Rust services under `frontend/src-tauri/src`.

Do not add new endpoints to `backend/app/main.py`; that FastAPI code is legacy archive material only.

## Testing and Debugging

### Frontend Debugging

**Enable Rust Logging**:
```bash
# macOS
RUST_LOG=debug ./clean_run.sh

# Windows (PowerShell)
$env:RUST_LOG="debug"; ./clean_run_windows.bat
```

**Developer Tools**:
- Open DevTools: `Cmd+Shift+I` (macOS) or `Ctrl+Shift+I` (Windows)
- Console Toggle: Built into app UI (console icon)
- View Rust logs: Check terminal output

### Audio Pipeline Debugging

**Key Metrics** (emitted by pipeline):
- Buffer sizes (mic/system)
- Mixing window count
- VAD detection rate
- Dropped chunk warnings

**Monitor via Developer Console**: The app includes real-time metrics display when recording.

## Platform-Specific Notes

### macOS
- **Audio Capture**: Uses ScreenCaptureKit for system audio (macOS 13+)
- **GPU**: Metal + CoreML automatically enabled
- **Permissions**: Requires microphone + screen recording permissions
- **System Audio**: Requires virtual audio device (BlackHole) for system capture

### Windows
- **Audio Capture**: Uses WASAPI (Windows Audio Session API)
- **GPU**: CUDA (NVIDIA) or Vulkan (AMD/Intel) via Cargo features
- **Build Tools**: Requires Visual Studio Build Tools with C++ workload
- **System Audio**: Uses WASAPI loopback for system capture

**First-time Windows setup gotchas** (each one caused a real build failure when set up from scratch):
1. **Rust toolchain must be reasonably current** (`rustup update stable`). An old toolchain (e.g. 1.73 from Oct 2023) fails to resolve the `cidre` git dependency (macOS-only, but Cargo still needs its manifest to compute the lockfile) with a confusing `no matching package named 'cidre' found` error that looks unrelated to the toolchain.
2. **libclang (LLVM) version matters — do NOT install the latest.** `bindgen` (used by `whisper-rs-sys`) breaks with very recent LLVM/Clang (seen with LLVM 22): anonymous nested structs/unions in `whisper.h` (e.g. `greedy`, `beam_search`) get bound differently, causing dozens of `no field 'X' on type 'whisper_full_params'` compile errors deep in the `whisper-rs` crate itself (not this repo's code). **Use LLVM 18.x** (tested working: 18.1.8) and set `LIBCLANG_PATH` to its `bin` directory.
3. **CMake is required** (compiles whisper.cpp/llama.cpp via `whisper-rs-sys`'s build script) and is not bundled — install separately and ensure it's on `PATH`.
4. **The `llama-helper` sidecar binary is not built automatically** by `pnpm run tauri:dev` (only the GPU-specific `dev-gpu.bat`/`build-gpu.bat` scripts do this). If `tauri.conf.json`'s bundled binary is missing, the build fails with `resource path 'binaries\llama-helper-<target-triple>.exe' doesn't exist`. Fix: build it manually and copy it in with the correct target-triple suffix:
   ```powershell
   cd llama-helper
   cargo build
   Copy-Item ..\target\debug\llama-helper.exe `
     ..\frontend\src-tauri\binaries\llama-helper-x86_64-pc-windows-msvc.exe
   ```
5. **PATH changes from installers (LLVM, CMake) don't apply to already-open terminals/IDE sessions** — open a fresh terminal (or restart VS Code) after installing them, or set the env vars inline for the current session:
   ```powershell
   $env:PATH = "C:\Program Files\CMake\bin;C:\Program Files\LLVM\bin;" + $env:PATH
   $env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
   ```
6. Opening `http://localhost:3118` in a regular browser (instead of the actual Tauri window) to "pre-warm" the Next.js dev compile is fine, but every Tauri-API-dependent component (anything using `@tauri-apps/api/event`'s `listen()` — 60+ call sites) will throw `Cannot read properties of undefined (reading 'transformCallback')` there, since the Tauri IPC bridge (`window.__TAURI_INTERNALS__`) only exists inside the real Tauri webview. This is expected and not a bug.

### Linux
- **Audio Capture**: ALSA/PulseAudio
- **GPU**: CUDA (NVIDIA) or Vulkan via Cargo features
- **Dependencies**: Requires cmake, llvm, libomp

## Performance Optimization Guidelines

### Audio Processing
- Use `perf_debug!()` / `perf_trace!()` for hot-path logging (zero cost in release)
- Batch audio metrics using `AudioMetricsBatcher` (pipeline.rs)
- Pre-allocate buffers with `AudioBufferPool` (buffer_pool.rs)
- VAD filtering reduces Whisper load by ~70% (only processes speech)

### Whisper Transcription
- **Model Selection**: Balance accuracy vs speed
  - Development: `base` or `small` (fast iteration)
  - Production: `medium` or `large-v3` (best quality)
- **GPU Acceleration**: 5-10x faster than CPU
- **Parallel Processing**: Available in `whisper_engine/parallel_processor.rs` for batch workloads

**Pending: parallelizing transcription (discussed, not implemented).** Both the live pipeline (`transcription/worker.rs`, `NUM_WORKERS = 1`, serial by design "to keep chronological emission order") and the offline retranscription loop (`retranscription.rs::transcribe_audio_file()`, segments transcribed one at a time in a `for` loop) currently process one audio segment at a time. Investigated whether this is actually necessary:
- **Whisper is already architected for safe concurrent use**: `WhisperEngine::transcribe_audio_with_confidence` (`whisper_engine.rs`) takes only a **read** lock on the shared `Arc<RwLock<Option<WhisperContext>>>` and creates a fresh, independent `WhisperState` per call (`ctx.create_state()`) — exactly the pattern whisper.cpp is designed to support multiple concurrent transcriptions against one loaded model. Raising `NUM_WORKERS` (live) or running several segments concurrently in the offline loop should give a real speedup, not just queue up on an internal lock.
- **Parakeet is not** — `ParakeetEngine::transcribe_audio` takes an exclusive **write** lock on a single shared `Arc<RwLock<Option<ParakeetModel>>>` and its `transcribe_samples` needs `&mut self`, so concurrent calls today just serialize on that lock regardless of how many workers/tasks you spawn. Would need restructuring (e.g. exposing the underlying `ort::Session`s in a way that supports concurrent `.run()` calls, which ONNX Runtime sessions generally allow) before parallelizing would help for Parakeet specifically.
- The frontend already has a `sequence_id`-based reorder buffer in `TranscriptContext.tsx` (built for out-of-order live delivery) — meaning the "must stay serial to keep order" constraint on `NUM_WORKERS` may be unnecessary; worth revisiting when this is picked up. For the offline path, order doesn't matter mid-flight anyway since results get sorted by timestamp before saving regardless (same pattern already used to merge mic+system chronologically).

### Frontend Performance
- React state updates batched via Sidebar context
- Transcript rendering virtualized for large meetings
- Audio level monitoring throttled to 60fps

## Important Constraints and Gotchas

1. **Audio Chunk Size**: Pipeline expects consistent 48kHz sample rate. Resampling happens at capture time.

2. **Platform Audio Quirks**:
   - macOS: ScreenCaptureKit requires macOS 13+, needs screen recording permission
   - Windows: WASAPI exclusive mode can conflict with other apps
   - System audio requires virtual device (BlackHole on macOS, WASAPI loopback on Windows)

3. **Whisper Model Loading**: Models are loaded once and cached. Changing models requires app restart or manual unload/reload.

4. **No Separate Backend Dependency**: Meeting persistence, transcription, and LLM features are handled by the Tauri app. Do not reintroduce the archived FastAPI backend as a supported requirement.

5. **Legacy FastAPI Security Context**: The archived FastAPI/CORS behavior is unsupported legacy code and must not be treated as a supported production API.

6. **File Paths**: Use Tauri's path APIs (`downloadDir`, etc.) for cross-platform compatibility. Never hardcode paths.

7. **Audio Permissions**: Request permissions early. macOS requires both microphone AND screen recording for system audio.

## Repository-Specific Conventions

- **Logging Format**: Rust logs should include enough module context to diagnose app behavior
- **Error Handling**: Rust uses `anyhow::Result`, frontend uses try-catch with user-friendly messages
- **Naming**: Audio devices use "microphone" and "system" consistently (not "input"/"output")
- **Git Branches**:
  - `main`: Stable releases
  - `fix/*`: Bug fixes
  - `enhance/*`: Feature enhancements
  - Current: `fix/audio-mixing` (working on audio pipeline improvements)

## Pending / Next Steps (as of 2026-08-07)

- **Calibrate diarization clustering threshold** (`diarization_engine/cluster.rs`, `MERGE_DISTANCE_THRESHOLD = 0.25`) against more real recordings — confirmed working on one 2-speaker test, but the threshold is an untuned starting point, not validated for over/under-splitting.
- **Parallelize transcription** — see the "Pending" note under Performance Optimization Guidelines → Whisper Transcription above. Whisper's engine layer already supports safe concurrent calls; Parakeet needs restructuring first. Not started.
- **Diarization Phase 3, part B**: true live diarization during recording (incremental clustering as speech arrives). Part A (offline speaker identification for live-transcribed meetings, via the new "Identify speakers" action) is done — see the Speaker Attribution section above.
- Minor UI polish not done: no persistent visual indicator (e.g. spinner on the "Transcribe"/"Enhance" button itself) when a retranscription is running in the background after the dialog has been closed — currently you only find out via the completion toast when it finishes.
- `write_retranscription_metadata()` (`retranscription.rs`) doesn't update `duration_seconds` when patching an existing `metadata.json` (only sets it when creating fresh metadata) — minor, metadata.json isn't the source of truth for duration anywhere in the UI today, but worth fixing if that changes.

## Key Files Reference

**Core Coordination**:
- [frontend/src-tauri/src/lib.rs](frontend/src-tauri/src/lib.rs) - Main Tauri entry point, command registration
- [frontend/src-tauri/src/audio/mod.rs](frontend/src-tauri/src/audio/mod.rs) - Audio module exports
- [frontend/src-tauri/src/database/mod.rs](frontend/src-tauri/src/database/mod.rs) - Local database module

**Audio System**:
- [frontend/src-tauri/src/audio/recording_manager.rs](frontend/src-tauri/src/audio/recording_manager.rs) - Recording orchestration
- [frontend/src-tauri/src/audio/pipeline.rs](frontend/src-tauri/src/audio/pipeline.rs) - Audio mixing and VAD
- [frontend/src-tauri/src/audio/recording_saver.rs](frontend/src-tauri/src/audio/recording_saver.rs) - Audio file writing

**UI Components**:
- [frontend/src/app/page.tsx](frontend/src/app/page.tsx) - Main recording interface
- [frontend/src/components/Sidebar/SidebarProvider.tsx](frontend/src/components/Sidebar/SidebarProvider.tsx) - Global state management

**Whisper Integration**:
- [frontend/src-tauri/src/whisper_engine/whisper_engine.rs](frontend/src-tauri/src/whisper_engine/whisper_engine.rs) - Whisper model management and transcription

**Speaker Attribution / Summary Templates** (added after this doc's initial version):
- [frontend/src-tauri/src/audio/transcription/worker.rs](frontend/src-tauri/src/audio/transcription/worker.rs) - `device_type_to_source()`, tags each transcript segment "mic"/"system"
- [frontend/src-tauri/src/summary/templates/](frontend/src-tauri/src/summary/templates/) - `loader.rs`, `defaults.rs`, `types.rs` - template resolution + validation
- [frontend/src-tauri/src/summary/template_commands.rs](frontend/src-tauri/src/summary/template_commands.rs) - Tauri commands for listing/saving/deleting templates
- [frontend/src/components/TemplateManager/TemplateManager.tsx](frontend/src/components/TemplateManager/TemplateManager.tsx) - shared template view/create/edit/delete UI
- [frontend/src/components/VirtualizedTranscriptView.tsx](frontend/src/components/VirtualizedTranscriptView.tsx) - renders the "You"/"Others"/"Speaker N" badge

**"Record Only" Mode / Offline Retranscription / Diarization Phase 2** (added after this doc's initial version):
- [frontend/src-tauri/src/audio/recording_preferences.rs](frontend/src-tauri/src/audio/recording_preferences.rs) - `transcribe_live` preference
- [frontend/src-tauri/src/audio/pipeline.rs](frontend/src-tauri/src/audio/pipeline.rs) - skips VAD/Whisper when `transcribe_live` is off; raw `mic_raw_sender`/`system_raw_sender` for the extra tracks
- [frontend/src-tauri/src/audio/incremental_saver.rs](frontend/src-tauri/src/audio/incremental_saver.rs) - generalized with a `base_name` param to save 3 tracks (`audio`/`mic`/`system`) instead of 1
- [frontend/src-tauri/src/audio/retranscription.rs](frontend/src-tauri/src/audio/retranscription.rs) - offline decode → VAD → transcribe → (diarize) → save pipeline; `transcribe_audio_file()`, `run_diarization()`, `find_dual_audio_files()`/`find_system_audio_file()`; also `run_speaker_identification()` - the standalone, no-re-transcription diarization path for live-transcribed meetings (Phase 3 part A)
- [frontend/src-tauri/src/audio/vad.rs](frontend/src-tauri/src/audio/vad.rs) - `get_speech_chunks_with_progress()` (offline VAD entry point), `ContinuousVadProcessor` (see the `speech_start_sample` timestamp gotcha above)
- [frontend/src-tauri/src/diarization_engine/](frontend/src-tauri/src/diarization_engine/) - `engine.rs` (model download/lifecycle, mirrors `parakeet_engine`), `model.rs` (fbank + ONNX embedding extraction), `cluster.rs` (agglomerative clustering), `commands.rs` (Tauri commands)
- [frontend/src/components/RecordingControls.tsx](frontend/src/components/RecordingControls.tsx) - record-mode dropdown ("Record & transcribe" / "Record only")
- [frontend/src/hooks/useRecordingStart.ts](frontend/src/hooks/useRecordingStart.ts) / [useRecordingStop.ts](frontend/src/hooks/useRecordingStop.ts) - start/stop lifecycle, mode-aware save flow
- [frontend/src/components/MeetingDetails/TranscriptButtonGroup.tsx](frontend/src/components/MeetingDetails/TranscriptButtonGroup.tsx) / [RetranscribeDialog.tsx](frontend/src/components/MeetingDetails/RetranscribeDialog.tsx) - "Transcribe"/"Enhance" UI, background-capable (closeable while processing)
- [frontend/src/components/MeetingDetails/DiarizeDialog.tsx](frontend/src/components/MeetingDetails/DiarizeDialog.tsx) - "Identify speakers" UI (Phase 3 part A) - no config, just triggers `run_speaker_identification()` and shows progress/result
- [frontend/src/components/DiarizationModelManager.tsx](frontend/src/components/DiarizationModelManager.tsx) - Settings panel for downloading the speaker-embedding model
