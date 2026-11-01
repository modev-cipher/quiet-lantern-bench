-- Fix A: Purge poisoned provider_request_cache rows from the legacy 250ms silence fallback.
-- Audio hash 9623afc24a77f9112c0fe5fbece3f9eac2f9a67b504a1d5918149c3974dff188 maps to the silence WAV.
DELETE FROM provider_request_cache
WHERE audio_hash = '9623afc24a77f9112c0fe5fbece3f9eac2f9a67b504a1d5918149c3974dff188';
