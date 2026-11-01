# Boundary export — unit 2 evidence (implementation + pins + S22)

Branch `feat/boundary-export`, base head `2feb9c19` (live probe: per-span
finals 250–320 ms, empty-end silently accepted, resume 3/3 same socket).
Spec: span-join investigation §5 (Candidate 3) + §9 pin list, owner ruling
O1. Command per phase: `CARGO_BUILD_JOBS=2 cargo test --lib voice_session::`.

## Implementation

`SessionRuntime::export_speech_boundary`: on the client VAD's SpeechEnd —
and at the SpeechStart defensive close of a missed SpeechEnd — the actor
calls `finalize_input()` (`realtimeInput.audioStreamEnd`) on the TRANSCRIBE
channel only. Fire-and-forget: no stream installed (re-dial in flight) is a
silent skip; a send refusal is a warn (`reason = "boundary_export_refused"`)
and a skip — the server's own VAD is the documented fallback segmentation.
Dialog leg, admission law, `FanoutAccount`, teardown honesty: untouched.
`MockChannel` grew `finalize_input` recording (`MockOp::AudioStreamEnd`) so
the pins assert count and ordering.

## Pins (§9's eight, all green both feature states)

1. `a_boundary_export_sends_one_end_per_span_and_each_cycle_attaches_forced`
2. `a_boundary_export_skipped_during_redial_still_settles_by_server_fallback`
3. `a_boundary_export_on_a_false_onset_settles_nothing_and_steals_no_neighbor`
4. `a_boundary_export_rides_a_barged_span_without_touching_the_interrupt`
5. `a_boundary_export_send_failure_never_blocks_the_mic`
6. `a_boundary_export_orphaned_by_a_stream_death_clears_honestly_at_resync`
7. `a_boundary_export_fires_at_the_defensive_close_before_the_next_spans_audio`
8. `a_boundary_export_keeps_rapid_spans_fifo_one_to_one`

Gates:
- default features: 125 passed; 0 failed; 3 ignored (1273 filtered out).
- `--features book-agent`: 137 passed; 0 failed; 3 ignored (1276 filtered out).
- `cargo clippy --all-targets -- -D warnings`: clean, both feature states.
- `cargo fmt --check`: pre-existing divergence at base head `2feb9c19`
  (system rustfmt version mismatch, hunks all in untouched code — verified
  by stashing this unit's diff and re-running); no new violations added.

## Sabotage S22

Defect: remove the SpeechEnd→`export_speech_boundary` call (the defensive
SpeechStart close left intact). Full raw transcript (defect diff, RED
output, restore, GREEN output) appended to the durable bundle
`~/.paseo/orchestration/banks/r2-voice-evidence/sabotage-transcripts.log`
under "Boundary-export sabotage S22 — unit 2, 2026-08-31".

- RED: 118 passed; 7 failed; 3 ignored — pins 1–6 and 8 red (every
  SpeechEnd-driven export assertion), pin 7 green as designed (it exercises
  the untouched defensive-close call site).
- Restore GREEN: 125 passed; 0 failed; 3 ignored.

Live before/after smoke and the O3 display-concatenation question are
UNIT 3 scope.
