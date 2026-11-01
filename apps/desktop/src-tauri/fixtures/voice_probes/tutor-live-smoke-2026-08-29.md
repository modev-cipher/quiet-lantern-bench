# Tutor live smoke — 2026-08-29 (F4 feedback round, owner-approved)

Key custody: VA3 vault (`voice_keys.vault`), never exported. Cost: two short
sessions, pennies. All model ids from `model_pins.rs`.

## 1. Dialog session — steered setup ACCEPTED live

Run: `cargo test --lib smoke_dialog_steered_setup -- --ignored --nocapture`

- Model: `models/gemini-2.5-flash-native-audio-preview-12-2025`.
- Setup carried `generationConfig.thinkingConfig.thinkingBudget: 0`,
  `inputAudioTranscription.languageCodes: ["ar-EG","en-US"]`, and the
  excerpt-term `customVocabulary` — `setupComplete` arrived, so the endpoint
  accepts every new field (a rejected setup kills the handshake).
- One text turn ("Say the single word: ready.") answered with transcript
  `"ready"`; whole test 1.84 s wall — the thinking-off latency is visible.

## 2. Transcription stream — WORKS, word-perfect on the owner's failing phrases

Run: `TRANSCRIBE_SMOKE_PCM=/tmp/probe_natural.pcm
cargo test --lib smoke_transcribe_live -- --ignored --nocapture`

- Model: `models/gemini-3.5-transcribe-live`, transcribe-mode setup (TEXT
  modality + the same hints/vocabulary) — `setupComplete` arrived: ACCEPTED.
- Probe: app kokoro TTS (`af_heart`, 24 kHz → 16 kHz), natural speech, 8.1 s:
  "Hello Gemini. Do you see the current SQL code? I want to fine tune the
  transformer model with self attention."
- Finalized segment received, verbatim:
  `"Hello Gemini, do you see the current SQL code? I want to fine-tune the
  transformer model with self-attention."`
  Both of the owner's session-1 failure phrases ("hello gemini" → "old
  jimmy", "sql" → "sequel") transcribe correctly on this stream.
- Interims streamed under `serverContent.interimInputTranscription` (ignored
  by design), finals under `serverContent.inputTranscription` — exactly the
  field the adapter forwards.

## 3. Measured trap: synthetic speech is VAD-invisible

espeak-ng probes (English AND Arabic, healthy amplitude, real-time pacing,
with and without `audioStreamEnd`, steered and bare configs) produced ZERO
server frames after `setupComplete` on BOTH models. The server VAD simply
never engages on robotic synthesis. Any future audio probe uses natural
speech (kokoro recipe above) or a human recording.

## 4. Owed to close F4's quality question

- Egyptian-Arabic + English CODE-SWITCH quality: kokoro has no Arabic voice,
  so this needs an owner-voiced probe — record ~20 s of natural Egyptian
  Arabic with embedded English terms, convert
  (`ffmpeg -i probe.wav -f s16le -ar 16000 -ac 1 probe.pcm`), run smoke 2,
  and compare against the same file with `TRANSCRIBE_SMOKE_DIALOG=1` (the
  dialog model's own transcription — the A/B that quantifies the second
  stream's win).
