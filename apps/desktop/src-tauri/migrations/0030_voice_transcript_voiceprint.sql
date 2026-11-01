ALTER TABLE voice_transcript ADD COLUMN voiceprint TEXT
  CHECK (voiceprint IS NULL OR length(voiceprint) = 40);
