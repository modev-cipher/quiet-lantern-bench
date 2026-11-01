DELETE FROM app_setting
WHERE key = 'default_model.tts'
  AND value IN ('xtts-v2', 'piper-en-us-lessac-medium', 'orpheus-3b', 'f5-tts');

DELETE FROM voice_profile
WHERE kind = 'preset'
  AND engine IN ('xtts-v2', 'piper-en-us-lessac-medium', 'orpheus-3b', 'f5-tts')
  AND id NOT IN (SELECT voice_profile_id FROM render_job);
