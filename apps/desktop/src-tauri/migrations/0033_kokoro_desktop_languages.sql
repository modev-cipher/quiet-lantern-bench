DELETE FROM voice_profile
WHERE kind = 'preset'
  AND engine = 'kokoro-82m'
  AND substr(preset_voice_id, 1, 1) IN ('e', 'f', 'h', 'i', 'p')
  AND substr(preset_voice_id, 2, 1) IN ('f', 'm')
  AND substr(preset_voice_id, 3, 1) = '_'
  AND id NOT IN (SELECT voice_profile_id FROM render_job);
