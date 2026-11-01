# Page-sight live smoke — 2026-08-30 (F7, owner-approved)

Vault key custody; pinned dialog model (`model_pins::TUTOR_LIVE_MODEL`).
Run: `VISION_SMOKE_PNG=<rendered page png> cargo test --lib
smoke_dialog_page_sight -- --ignored --nocapture` (+`VISION_SMOKE_CAP=1`
for the cap probe).

## Sight — PROVEN, unambiguously
A real rendered page PNG (940 KB, 705×996, from the app's own page cache)
went over `realtimeInput.video` mid-session; with an EMPTY excerpt and a
neutral prompt (nothing text-side to answer from), the model replied:
"I see a handwritten page with text and drawings." — pixels, not text.
(Run 1 used the standard tutor context and was discarded as ambiguous: its
excerpt itself named a loss-curve figure.)

## 2-minute video-cap question — answered
Docs cap "sessions with video" at 2 minutes (vs 15 audio-only). Measured
twice: the session stayed alive 159.7 s and 161.6 s after the still (probe
ceiling ~155 s + drain), pings answered throughout. A single page still
does NOT reclassify the session. Long-idle behavior beyond 2:40 untested;
the tutor's renewal path covers expiry regardless.

## Setup facts riding this smoke
`turnCoverage: TURN_INCLUDES_ALL_INPUT` and
`mediaResolution: MEDIA_RESOLUTION_LOW` were in the accepted setup frame —
the endpoint takes both alongside the F4 steering fields.

## ALL_INPUT audio-dialog smoke (review finding 5) — PASSED, with a bonus A/B
`TRANSCRIBE_SMOKE_DIALOG=1` ran the natural-speech probe through the DIALOG
session under the new `turnCoverage: TURN_INCLUDES_ALL_INPUT`: audio was
heard, VAD segmented normally, input transcription streamed — no echo/garble
regression of the F6 class observed on this probe. Bonus: the dialog model
transcribed "sequel code" and "find tune the transformer" on the SAME audio
the transcribe-live stream rendered word-perfect ("SQL code", "fine-tune") —
the quantified A/B behind the second-stream ruling.
