# txtCaster — System Architecture

Status: v1.0 (design) · Date: 2026-07 · Targets: Windows x64, Linux x64 (MVP); macOS in v1.0

---

## 1. Architecture Overview

txtCaster is split into two cooperating processes:

1. **Desktop App (Tauri 2, Rust + React/TypeScript)** — UI, orchestration, job queue,
   SQLite storage, model download manager, FFmpeg invocation, hardware profiling.
2. **ML Engine (Python 3.11 sidecar)** — all ML inference: document parsing, layout
   analysis, LLM script generation, MLLM figure explanation, TTS, forced alignment.
   Packaged with PyInstaller; lifecycle managed by the Tauri process (spawn/health-check/kill).

Deployment hosts: desktop sidecar today; cloud job runner planned — engine contracts identical
(release order: ROADMAP.md §Release milestones; platform spec kept outside this repo).

Rationale:
- All viable ML libraries (Docling, llama-cpp-python, Kokoro, Chatterbox, whisperX) are Python.
- A sidecar isolates crashes, memory leaks, and VRAM pressure from the UI.
- Tauri gives ~10 MB base binaries, native performance, first-class sidecar support,
  and ~50% less RAM than Electron (2026 benchmarks — see TECH_STACK.md).
- FFmpeg runs as a separate subprocess invoked by Rust (media rendering is not Python's job).

```
┌────────────────────────── Tauri Desktop App (Rust core) ──────────────────────────┐
│                                                                                    │
│  ┌───────────── React/TS UI (WebView) ─────────────┐                              │
│  │ Library · Scope Picker · Mode/Voice Config      │                              │
│  │ Model Manager UI · Job Progress · Preview Player │                              │
│  └───────────────────────┬──────────────────────────┘                              │
│                    Tauri IPC (commands/events)                                      │
│  ┌───────────────────────┴──────────────────────────────────────────────────────┐ │
│  │ Rust Core                                                                     │ │
│  │  · JobOrchestrator (state machine, checkpoints, cancel/resume)                │ │
│  │  · HardwareProfiler (VRAM/RAM/CPU probe, Compute Score, offload calculator)   │ │
│  │  · ModelManager (catalog, downloads w/ resume+SHA256, disk layout)            │ │
│  │  · SidecarSupervisor (spawn, health, restart, port negotiation)               │ │
│  │  · MediaRenderer (FFmpeg pipeline: MP3 encode, loudnorm, chapters)            │ │
│  │  · Storage (SQLite via sqlx, content-addressed artifact cache)                │ │
│  └───────────────────────┬──────────────────────────────────────────────────────┘ │
└──────────────────────────┼─────────────────────────────────────────────────────────┘
                 HTTP/JSON (localhost, random port, bearer token)
┌──────────────────────────┴──────────────── Python ML Sidecar (FastAPI) ────────────┐
│  /ingest    PDF/EPUB → pages, text blocks, TOC, page renders (PNG)                 │
│  /layout    figure/table/chart regions with bounding boxes (Docling / fast path)   │
│  /script    Narrator or Teacher script segments (llama.cpp GGUF LLM)               │
│  /vision    figure captions/explanations (the cast's MLLM + mmproj)                │
│  /tts       audio per segment (Kokoro / Chatterbox / plugin backends)              │
│  /align     word-level timestamps (whisperX forced alignment)                      │
│  /verify    optional TTS-output verification (ASR round-trip check)                │
│  /health, /models/load, /models/unload, /cancel                                    │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

---

## 2. End-to-End Data Flow

```
Import (PDF/EPUB)
  → Ingest: text blocks + coordinates, TOC/chapters, page PNG renders
  → Scope Selection (user): chapters or page ranges
  → Layout Analysis (scoped pages only): FigureRegion{page, bbox, type}
  → Script Generation:
      Narrator: normalization pass (numbers, math, abbreviations) → verbatim segments
      Teacher : chunked map-reduce w/ rolling summary → lecture segments
      Both    : [FIGURE:id] anchors inserted at figure positions;
                /vision captions feed the LLM for figure explanation segments
  → TTS: per-segment synthesis (parallel worker pool), cached by content hash
  → Forced Alignment: whisperX word timestamps per segment
  → Render:
      MP3: concat segments + chapter markers (FFmpeg, libmp3lame)
  → Export + report (skipped items, verification flags)
  → In-app follow-along view: page renders + word-level alignment map drive
    synchronized text highlighting during playback (no video file is produced)
```

Every stage writes artifacts to the content-addressed cache and a checkpoint row to
SQLite. Any stage can resume mid-job. Editing the script invalidates only the affected
segments downstream (TTS/align/render for those segments only).

---

## 3. Components

### 3.1 JobOrchestrator (Rust)
- Owns the pipeline state machine (see PIPELINE_SPEC.md §1 for states).
- One active *heavy* job at a time (ML stages); light stages (ingest of a second book)
  may run concurrently if RAM allows.
- Emits progress events to UI: `{job_id, stage, pct, eta_seconds, current_item}`.
  `pct` is the inclusive stage-completion **fraction in [0.0, 1.0]** — every
  emitter uses `done/total`; UI percent displays multiply by 100 exactly once
  at the presentation layer.
- Checkpoint after every completed segment/stage; cancel is cooperative
  (sidecar `/cancel` + kill switch after grace period).
- **OOM recovery loop**: on sidecar OOM/crash during a stage → restart sidecar →
  reduce resource plan (fewer GPU layers / smaller batch) → resume from checkpoint →
  notify UI ("Reduced GPU usage to continue").
- **Graceful shutdown**: window close / OS exit request → the orchestrator
  latches a process-lifetime `shutting_down` flag **before** any
  cancellation or state transition. Setting the latch stops dispatching
  queued work, disables SidecarSupervisor restart/backoff, prevents new
  model loads and FFmpeg launches, and makes all later events from
  already-running stage workers non-authoritative for recovery decisions.
  Then, in order: authenticated `/cancel` for active jobs (cooperative
  grace ≤ 5 s) → running stages settle atomically as PAUSED at their last
  committed checkpoint (ONBOARDING_UX.md §4 "Job paused — will resume next
  launch") → the complete sidecar and FFmpeg **process trees** are
  terminated (terminate grace ≤ 2 s, then force-kill and reap; Unix:
  process group established in the child **before exec** (`setpgid`),
  group-directed SIGTERM→SIGKILL; Windows: child created suspended →
  assigned to a non-breakaway Job Object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` → resumed, so no descendant can
  spawn outside containment; all Job Object handles close before the
  deadline). Overall close deadline ≤ 10 s. Killing by executable name,
  port lookup, or PID file is forbidden — retained child handles are
  authoritative.
  Shutdown transition table: CREATED/QUEUED → unchanged (re-queued by
  `resume_on_launch`); active stage → PAUSED; job PAUSED by an
  already-committed recovery → stays PAUSED with its persisted degraded
  plan; CANCELLED/FAILED/DONE → unchanged. Shutdown preserves only
  degradation **committed before the latch**: a recovery transaction not
  yet committed when the latch is set must abort without changing the
  plan, checkpoint, revision, or `recovery_log`. Recovery commit and
  latch acquisition are serialized by the same mutation gate, and
  recovery re-checks the latch immediately before commit; recovery
  writes are conditioned on the active job state, not only the plan
  revision.
  Stream/process failures observed *after the latch* are the expected
  consequence of termination — they must **never** enter the OOM recovery
  loop or degrade any persisted resource plan. Only genuine runtime
  failures (ERR_OOM, unexpected sidecar death while the app is live) walk
  the degradation ladder. If the OS kills the app before settlement
  completes, `resume_on_launch` re-queues jobs from their last checkpoint
  exactly as after `kill -9` — in both paths shutdown adds **no
  additional** plan or recovery-log mutation, while any plan and
  `recovery_log` state committed before the latch is preserved.

### 3.2 HardwareProfiler (Rust)
See HARDWARE_PROFILER.md. Produces `HardwareProfile` + `ComputeScore`, plus a
per-model, per-stage `ResourcePlan` (stage-specific shapes — GGUF, TTS,
align — in DATA_MODELS.md §2.6).

### 3.3 ModelManager (Rust)
- Loads versioned catalog JSON (bundled copy + optional user-refreshed copy).
- Downloads: HTTP range-resume, SHA256 verification, disk-space pre-check.
- Storage: `<models_dir>/<task>/<model-id>/`, where `<models_dir>` defaults to
  `~/.local/share/io.github.meedoomostafa.txtcaster/models/` — the bundle identifier from
  `tauri.conf.json`, not the product name (XDG; `%LOCALAPPDATA%\\txtcaster-data` on Windows). The
  location is overridable through the `storage.models_dir` setting (R-AC).
  `storage::paths::resolve` is the intended single source. **As of R-AC part 1 that is not
  yet true of every caller**: the free-space probes resolve, while `ModelManager` and the
  stage registry still take the default root built in `run()`. The remaining sites are
  counted in `storage::paths::tests::every_directory_still_built_by_hand_is_declared_here`,
  which fails the build both when a new one appears and when a migrated one is left in the
  list, and they go to zero in R-AC part 2.
- **Custom model import (v1.1+)**: user may register any local GGUF (LLM/MLLM) or
  supported TTS model directory; catalog entry synthesized with `source: "custom"`,
  fit badge computed the same way. (Deferred past MVP — see ROADMAP.md.)
- Never blocks selection: exposes fit badges (🟢🟡🟠🔴) computed from ResourcePlan.

### 3.4 SidecarSupervisor (Rust)
- Spawns the PyInstaller-packaged engine with `--port 0 --token <random>`;
  reads the negotiated port from stdout handshake line.
- Health-check every 5 s (5 s request timeout); restart only after 6 consecutive
  failures — a bounded ~30–60 s unresponsiveness tolerance (60 s when each
  request runs to its timeout) so a sidecar busy with heavy CPU-bound ML work
  is not killed by transient event-loop starvation. Process exits are detected
  promptly via `child.wait()` raced against the health request. Auto-restart
  with backoff; surfaces "engine restarting" to UI.
- Passes ResourcePlan per model-load call (`/models/load {model_id, n_gpu_layers, ctx, ...}`).

### 3.5 ML Engine (Python)
- FastAPI over localhost only; bearer token required on every request.
- **Model residency policy**: models are loaded/unloaded per stage under a VRAM budget
  (see §5). Registry pattern: `LLMBackend`, `VisionBackend`, `TTSBackend`, `Ingestor`,
  `LayoutEngine` are abstract interfaces → new engines are plugins.
- Long operations stream progress via Server-Sent Events; all endpoints accept
  `job_id` for cooperative cancellation.

### 3.6 MediaRenderer (Rust + FFmpeg)
- MP3: concat WAV segments → loudness normalize (EBU R128, `loudnorm`) → libmp3lame
  V0/320k; embeds chapter markers + metadata (ID3).
- Video export is out of scope: txtCaster is audio-first. Page renders (PNG) and the
  word-level `AlignmentMap` are consumed by the **in-app follow-along view** (reading
  view with synchronized word highlighting during playback) — not by a video encoder.

### 3.7 VoiceAgentHarness (Rust — Phase 5+, see VOICE_AGENT.md)
- Owns the real-time voice loop for the Interactive Voice Tutor: mic capture
  (`cpal`, OS realtime callback thread) → lock-free SPSC ring → tokio task →
  resample (`rubato`) → `SessionChannel` (WebSocket to a user-configured
  provider, or the local sidecar in Phase 6).
- Two planes over one socket: DATA (PCM audio up/down, adaptive ~150 ms jitter
  buffer on output) and CONTROL (JSON events → `ChannelEvent` stream →
  `ToolDispatcher`). Audio never touches the WebView; the UI receives only
  control-plane Tauri events (transcripts, session state). The two-plane
  contract is per-`SessionChannel`, not per-socket: a composed channel
  (VOICE_AGENT §3.2) multiplexes several provider connections (STT/LLM/TTS
  slots) behind the same interface and owns their lifecycle internally —
  connection failures, cancellation, and backpressure of individual slots are
  the channel's responsibility and surface as ordinary `ChannelEvent`s.
- Barge-in: atomic generation counter checked by the output callback (perceived
  stop < 20 ms) + async provider cancel.
- Voice tools execute **locally only** and mutate playback state — never
  `render_job`/checkpoint rows. Tool-tier policy is a hard invariant (§8).
- A Phase-6 local voice session registers as a **VramLease** (equivalent to a
  heavy job): batch jobs queue behind it; the SidecarSupervisor suspends
  restart-on-health-failure while a voice session is active (liveness via WS
  ping/pong instead).

### 3.8 Storage (Rust)
- SQLite (WAL mode) for entities (DATA_MODELS.md).
- Content-addressed artifact store: `cache/<sha256[0..2]>/<sha256>` for page renders,
  audio segments, alignment JSON. Key = hash of (input content + model id + params),
  so identical re-runs are free and edits invalidate minimally.

---

## 4. IPC Contracts

### 4.1 UI ↔ Rust (Tauri commands — selected)
```
import_book(path) -> BookSummary
get_chapters(book_id) -> [Chapter]
create_job(book_id, scope, mode, voice_profile_id, model_overrides?) -> JobId
job_events(job_id) -> stream ProgressEvent
cancel_job(job_id), pause_job(job_id), resume_job(job_id)
list_reviewable_segments(job_id) -> [SegmentSummary]     # flagged/edited/worst-listed only (A2)
resynthesize_segment(job_id, segment_id, text?)          # text = null reverts (A2)
rerender_job(job_id)                                     # heals a stale export (A2)
get_catalog() -> [CatalogEntry + FitBadge + ResourcePlan]
download_model(model_id) -> stream DownloadProgress
run_benchmark() -> ComputeScore
export(job_id, format: mp3, path)
```

### 4.2 Rust ↔ Sidecar (HTTP/JSON — canonical shapes in PIPELINE_SPEC.md)
All requests: `Authorization: Bearer <token>`, `X-Job-Id` header for cancellable ops.
Responses stream SSE progress for long ops, final message carries the artifact manifest
(paths into the shared cache directory — large payloads never travel over HTTP).
Exception: `/tts` streams one `result` event per finished segment (persisted
incrementally by the orchestrator) and ends with a small `done` summary instead
of an aggregate manifest (see PIPELINE_SPEC.md §6).

---

## 5. Memory / VRAM Budgeting & Model Residency

Budget = `min(VRAM_detected, user_cap) − reserved_ui_margin(512 MB)`.

That budget is the **VRAM** pool only. The host-RAM ceiling is a separate gate on the
CPU-resident bytes of a plan (HARDWARE_PROFILER.md §4); the two pools are never summed
(§6.1). Two emission paths currently reach the engine without passing that gate, and it
reads host memory rather than a cgroup limit; R-AM (TASKS.md) is the task that will close
both. Not yet implemented — treat the gate as governing the main path only.

Residency schedule (per pipeline run):
```
Stage           Models resident
ingest          none (CPU parsing only)
layout          layout model (~0.5–2 GB) — unloaded after stage
vision          the cast's MLLM + mmproj — unloaded after stage
script          LLM (largest consumer; ctx sized to chapter chunk)
tts             TTS model
align           whisperX (small/int8 by default)
```
- On ≥12 GB VRAM the orchestrator MAY keep LLM+TTS co-resident (pipelined stages).
- On ≤8 GB: strictly sequential load/unload with explicit CUDA cache flush between stages.
- **Residency ownership**: a single engine-side `ResidencyManager` owns every
  manager-tracked model (llm, vision, tts, align) behind refcounted leases keyed
  `(task, model_id, variant)` — so one `model_id` used both as a text LLM and as a
  vision-capable LLM (mmproj attached) is two distinct residents, never shared.
  It enforces admission (`max_residents`, `coresident_tasks`), unload-before-load,
  an eviction priority, and teardown *completing before the slot frees*. A stage
  holds a lease for its duration (a parallel worker pool shares one instance);
  the last release unloads unless the resident is pinned (explicit
  `/models/load`) or its task has an idle-timeout warm window (interactive
  `/tts` preview and per-segment re-synthesis). A pinned resident yields its
  slot only to an explicit `/models/unload`, which first waits up to
  `UNLOAD_LEASE_DRAIN_S` for active leases to drain and returns `ERR_BUSY`
  rather than freeing a model a stage is still generating with.
  `ERR_BUSY` is the contract's "come back later", so the orchestrator answers it
  with a bounded same-plan backoff sized from `RESERVATION_GRACE_S`, never with
  plan degradation (PIPELINE_SPEC.md §1).
  Layout is deliberately outside the manager: its Docling converter is built per
  request and dropped, so it is ephemeral by construction (DATA_MODELS.md §1).
- **Teardown is explicit, never GC-driven.** Every backend `unload()` is idempotent
  and frees each native handle it created, including handles it passed to another
  library (e.g. the llama.cpp mmproj/CLIP context, whose upstream chat handler
  exposes no `close()`). A torch cache flush is an additional step, never the
  mechanism — the shipped GGUF GPU path is a Vulkan build, where the torch
  allocator is not involved at all.
- **Verification**: the orchestrator samples free VRAM at stage entry and after
  teardown and records the delta per stage in the completion report
  (`vram_free_mb_delta`), warning when a stage leaves memory unreleased. The probe
  is best-effort — backends that do not report free VRAM record no delta and never
  block the pipeline.
- On hybrid plans (🟡/🟠): llama.cpp `n_gpu_layers` split per HARDWARE_PROFILER.md;
  TTS falls back to CPU with reduced batch instead of failing.

---

## 6. Flexibility Policy (architectural invariants)

1. **Advisory, never blocking.** Every catalog model is runnable on every machine.
   The only hard stop is a confirm dialog on a 🔴 fit — defined in
   HARDWARE_PROFILER.md §3: no GGUF placement fits the separate RAM/VRAM
   budgets with required headroom, or a non-GGUF model fits neither the
   GPU-plan VRAM budget nor the CPU-plan RAM budget (evaluated
   independently). RAM and VRAM are separate pools; they are never summed,
   and stage footprints are never summed across the (sequential) pipeline —
   fit badges are always per-stage. The optional ≥12 GB co-residency
   optimization (§5) is gated on its own combined live-memory check at
   runtime and falls back to sequential load/unload; it never affects
   badges or persisted plans.
2. **Concrete estimates.** Warnings always include an ETA computed from the
   micro-benchmark × scope size — never vague ("this may be slow").
3. **Hybrid execution is first-class.** CPU+GPU layer split is the default mechanism,
   not a fallback hack.
4. **Overnight mode.** 🟠/🔴 selections offer "queue and run unattended" with
   pause/resume and OS sleep-inhibit.
5. **Per-job overrides.** Model/voice/mode selectable per job; global defaults only
   seed the form.
6. **Capability-provider freedom (owner directive, 2026-08).** Every capability
   the product uses — TTS, STT, LLM/MLLM — can be served **100% locally or
   by a BYOK cloud provider**, at the user's choice. The uniform mapping rule:
   **any configured model or key is mappable to any role** whose required
   capability it provides — Narrator casts, Teacher casts, and live voice
   sessions alike. Roles bind to providers at exactly two points, and both
   points draw from the same pool (installed local models ∪ configured BYOK
   keys/endpoints, filtered by capability):
   - **Batch casts**: the per-job override / task default chain
     (`model_overrides_json`, Settings → Models defaults, then automatic
     resolution — the vision stage follows the cast's LLM, MODEL_CATALOG §4).
   - **Live voice sessions**: channel and slot configuration
     (VOICE_AGENT §3, §3.2).
   No model, key, or endpoint is ever reserved to one role; nothing is ever
   hard-blocked (invariant 1 applies — advisory labels, fit badges, and
   consent gates inform, they do not restrict). Cloud participation in a
   batch cast is **per-stage, explicit, and opt-in at cast time**
   (ONBOARDING_UX §3); the offline-local path remains the default everywhere.

---

## 7. Error Handling & Degradation Matrix

| Failure | Behavior |
|---|---|
| Sidecar crash / OOM | Restart → reduce ResourcePlan → resume from checkpoint |
| Model download interrupted | Range-resume; SHA256 re-verify on completion |
| Scanned/image-only PDF | Detected at ingest; route to OCR path (Docling OCR); warn about time |
| DRM-protected file | Detect, refuse with clear message (legal), no partial processing |
| LLM output malformed (Teacher) | Retry with stricter prompt ×2 → fall back to Narrator for that chunk, flag in report |
| TTS mispronunciation/hallucination | Optional /verify pass (ASR round-trip WER per segment) flags segments for user review |
| The cast's model cannot read images (no `figure_explanation`, mmproj missing, or a cloud provider without image input) | Insert generic cue ("See the figure on this page") — feature degrades, job continues |
| Layout artifacts unusable (not downloaded, incomplete, or corrupted) | Layout stage completes with zero regions, `layout_skipped` flagged in report — MP3 jobs unaffected |
| Export no longer matches the job's chapters (crash mid-render, an edited sentence, or a marker-semantics change) | At job open the recorded `export.chapter_digest` is compared with the current `chapter_render` set; a mismatch refuses playback (`get_media_url` errors, the shelf hides the play control) and the Jobs panel offers a resumable re-render. Never silently serves the stale file |
| No GPU | All stages CPU; catalog recommendations shift tiers automatically |
| Disk full mid-job | Pause job, surface actionable error with required space |
| Voice: provider outage / WS drop (transient) | Reconnect with backoff; replay buffered mic audio; session context restored from transcript journal |
| Voice: session-level expiry (provider GoAway / token expiry) | Renew session; re-inject rolling summary + playback position; discard (not replay) stale mic buffer |
| Voice: key revoked / quota exhausted (401/402/429) | Pause session with actionable error; transcript preserved; playback controls remain fully local |
| Voice: reconnect exhausted (>15 s) | Degrade to non-interactive mode — local playback tools stay functional, voice input disabled, one-tap retry |

---

## 8. Security & Privacy

- Sidecar binds localhost only; random port + per-session bearer token.
- Media streamer (Rust core) binds localhost only; random port + per-session
  token (query param — media elements cannot send headers); serves only files
  explicitly registered from the exports/cache directories (canonicalized,
  root-checked). Exists because WebKitGTK's GStreamer pipeline cannot read
  Tauri's custom asset scheme for `<audio>` on Linux; also provides
  HTTP Range support for seeking large audio without loading it into memory.
- LAN sender (send-to-phone) is the one user-initiated inbound listener: it
  binds only while the send dialog is open and dies with it. TLS 1.3 with a
  fresh per-session self-signed certificate whose SHA-256 fingerprint travels
  inside the QR code, so the phone pins the exact cert — no TOFU, no CA.
  Pairing needs a single-use 128-bit secret carried only in that QR; failed
  attempts are rate-limited, and connections from non-private addresses are
  dropped before the handshake. It serves only the exports the user selected
  for that session (resolved through the exports-root guard), keeps no state
  beyond in-memory session tokens, and never broadcasts — the QR on screen is
  the only discovery channel. Wire contract fixed by the phone's receive lane
  (`cast-client` transfer proto); the desktop conforms, never the reverse.
  (`txtCaster-mobile` transfer proto); the desktop conforms, never the reverse.
- No telemetry. Network access only: model downloads (user-initiated), optional
  catalog refresh (user-initiated), and — if the user explicitly enables them and
  supplies their own API keys — opt-in BYOK voice sessions (WSS) and opt-in BYOK
  cloud cast stages (per-stage, chosen at cast time — §6 invariant 6,
  ONBOARDING_UX §3). All over HTTPS/WSS with checksum verification where
  applicable. The send-to-phone LAN listener (bullet above) is inbound and
  local-network only; nothing it serves leaves the user's own network.
- User documents never leave your device **except** through the two explicit
  BYOK opt-ins: the optional Voice Tutor (sends the current chapter's text
  excerpts and your microphone audio to the provider you configure) and any
  cast stage you explicitly point at a cloud provider at cast time (sends that
  stage's inputs — book text for scripting, figure images for visuals, script
  text for TTS — to that provider). Anything that sends book-derived content
  off-device is gated on per-first-use-per-book consent naming the destination
  host; providers may retain data per their policies (linked in settings, with
  every remote endpoint listed uniformly as a provider). Cache directory
  permissions 0700.
- Model files verified by SHA256 from the signed catalog before load.

### 8.1 Voice Tutor security invariants (Phase 5+, see VOICE_AGENT.md)
- **API keys**: stored in the OS keychain (`keyring`), zeroized in memory
  (`zeroize`), loaded only at session start, held only inside the channel struct.
  Never in SQLite, never in logs, never in `Debug` output (redacting impls
  mandatory), UI shows `…last4` only with no reveal-after-save. Keys are stored
  **per base URL**: a custom endpoint can never receive the key stored for a
  named provider. Linux without Secret Service: passphrase-encrypted file or
  session-only keys — never plaintext. Each stored key carries a validation
  state (**active / invalid / out-of-quota**) refreshed by a capability-cheap
  authenticated probe — never a billable heavy call — on settings entry and
  on explicit user refresh only, never by ambient polling (VOICE_AGENT §7).
- **Custom base URLs**: HTTPS-only (explicit localhost exempt); first-use
  interstitial displays the hostname and what will be sent; custom URLs are
  confined to the dedicated "OpenAI-compatible" channel type.
- **Tool-tier policy (hard invariant)**: the ToolDispatcher must never expose
  filesystem paths, network requests, shell execution, model downloads, or
  settings mutation to LLM tool calls. Tier 0 = read-only; Tier 1 =
  reversible playback/presentation-state mutations (auto-allowed, rate-limited
  ~5 calls/10 s, args validated against strict schemas with bounded numerics;
  presentation state = client-local, reversible, never leaves the device,
  never touches book content — VOICE_AGENT §5); Tier 2 =
  anything with side effects beyond playback/presentation state — requires
  explicit per-invocation user confirmation, forever. Book excerpts are
  injected as clearly delimited untrusted *data*, never into the instruction
  slot.
- **ExcerptSelector**: the single code path allowed to read book text into a
  voice channel, with a hard byte cap; unit tests assert no other voice-path
  module touches page text.
- **Privacy defaults**: push-to-talk by default (open-mic is opt-in), persistent
  in-app recording indicator, transcripts cascade-delete with the book, and a
  retention setting is Keep until deleted (default) or Never (F11 D-8 ruled (c)).

---

## 9. Key Risks & Mitigations

| Risk | Mitigation |
|---|---|
| Math/technical notation mispronounced by TTS | Normalization pass in script stage (LLM rewrites formulas to speech); /verify flags residuals |
| Long chapters exceed LLM context | Chunked map-reduce with rolling summary; ctx budgeted by profiler |
| Python packaging size/fragility (PyInstaller) | CI builds per-OS; smoke test suite runs packaged binary; models never bundled |
| Copyleft ingest engine | Resolved: PDFs on pdfium (#49), EPUBs on the pure-Python reflow adapter (#26); the reference engine is an optional `bench` group behind the `Ingestor` interface |
| whisperX alignment drift on synthetic speech | Kokoro emits native timestamps (preferred when available); whisperX as universal fallback |
| Catalog staleness (model churn is fast) | Catalog is data, not code; refreshable independently of app releases |
| Tauri WebView inconsistencies (Win/Linux) | Keep UI to well-supported web APIs; e2e tests on both platforms |

---

## 10. Extensibility Points

- `TTSBackend` / `LLMBackend` / `VisionBackend` Python interfaces → add engines
  (CosyVoice2, Fish Speech, IndexTTS-2, Ollama-as-backend) without touching pipeline.
- `Ingestor` interface → add formats (DOCX, HTML, MOBI).
- Catalog JSON → add models without code changes.
- Follow-along view themes → highlight styles are template-driven.
