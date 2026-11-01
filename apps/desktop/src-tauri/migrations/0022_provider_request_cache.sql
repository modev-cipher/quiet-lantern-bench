-- S-4 (Hermes MF6): request-identity cache for cloud TTS. The key is the
-- semantic request identity — provider, canonical host, model, voice, exact
-- text, adapter revision — so an identical paid request is answered from the
-- ordinary wav cache instead of re-synthesized. Distinct from the
-- content-addressed wav store (which deduplicates OUTPUT bytes only).
CREATE TABLE provider_request_cache (
    request_key TEXT PRIMARY KEY,
    audio_hash TEXT NOT NULL,
    wav_bytes INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    words_json TEXT,
    created_at INTEGER NOT NULL
);
