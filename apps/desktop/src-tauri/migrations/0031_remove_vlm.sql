DELETE FROM app_setting WHERE key = 'default_model.vlm';

UPDATE render_job
SET model_overrides_json = json_set(model_overrides_json, '$.explain_figures', json('false'))
WHERE json_valid(model_overrides_json)
  AND (json_type(model_overrides_json, '$.vlm') = 'null'
       OR json_type(model_overrides_json, '$.vision_binding') = 'null');

UPDATE render_job
SET model_overrides_json = json_remove(model_overrides_json, '$.vlm')
WHERE json_valid(model_overrides_json)
  AND json_type(model_overrides_json, '$.vlm') IS NOT NULL;

UPDATE render_job
SET model_overrides_json = json_remove(model_overrides_json, '$.vision_binding')
WHERE json_valid(model_overrides_json)
  AND (json_type(model_overrides_json, '$.vision_binding') = 'null'
       OR json_extract(model_overrides_json, '$.vision_binding.kind') = 'local');
