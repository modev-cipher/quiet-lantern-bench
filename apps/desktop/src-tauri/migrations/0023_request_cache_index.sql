-- Hermes R2 fold-in 6: dead request-cache rows must be findable when cache
-- GC evicts their wav.
CREATE INDEX idx_provider_request_cache_audio_hash
    ON provider_request_cache (audio_hash);
