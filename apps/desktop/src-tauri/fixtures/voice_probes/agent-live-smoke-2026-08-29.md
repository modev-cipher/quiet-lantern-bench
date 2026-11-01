# Book Agent live smoke — conformance record

Run: 2026-08-29, hand-dispatched (charter §9.5 manual-only law), owner-authorized.
Model: `models/gemini-2.5-flash-native-audio-preview-12-2025` (the B-2 dated pin).
Key: owner's Tutor pool key via VA3 vault custody (no key bytes surfaced).
Harness: `agent_smoke::smoke_agent_conformance` (crate root) (`#[ignore]`,
key-optional). Budget: 42.4 s wall-clock, 7 tool turns (cap 50), transcript
258 chars — Flash-class pennies on the owner's tutor key (ledger note).

## Consent flow (first live proof)
`authorize_agent_egress` → ConsentRequired → `record_agent_consent` (displayed
hosts re-verified) → Authorized. PASS.

## Probe 1 — Arabic variant law (§3.5, T5a)
Question (Arabic) about the stars. The live model emitted
`{"queries":["النجوم","نجم"]}` — broken plural AND singular as separate
variants, exactly the prompt law's mandate for the spike-proven Snowball
misses. **PASS.**

## Probe 2 — quote-order oracle (§9.4)
Exact-quote request for the qandarun needle. `search_book` preceded every
quoted token in the output transcription (the ordering oracle); no quoted
token from memory. **Order: PASS.** Span fidelity: the spoken quote carried
the needle fragments ("of the qandarun alloy") but the full contiguous span
check read PARTIAL — output transcription chunking makes exact-span matching
noisy at this oracle; re-run with a longer settle window before treating as a
model fault.

## Probe 3 — python-genai#1894 speculative speech
`search_book` answer withheld 3 s under NON_BLOCKING; zero assistant content
during the hold. **NOT REPRODUCED** (one trial, 3 s hold). The landmine stays
recorded on the model pin; one clean trial does not retire it.

## Unplanned finding — handle mutation (owner note)
The model twice appended coordinate-like suffixes to opaque handles
(`<32-hex>:1:66`) when calling `open_passage`/`highlight_passage`. The typed
`unknown_handle` refusal held, the protocol never stalled, and the model fell
back to quoting from the snippet (charter-legal). Cheap mitigation for the
next prompt pass: "pass the handle exactly as returned, unmodified."
Also observed: one garbled variant (`كمHل`) among otherwise-correct Arabic
variants — leniently parsed as data, harmless.
