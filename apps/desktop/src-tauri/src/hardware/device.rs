use serde::{Deserialize, Serialize};

pub const PLAN_REVISION: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceBackend {
    Cpu,
    Cuda,
    Rocm,
    Vulkan,
    Metal,
}

impl DeviceBackend {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceBackend::Cpu => "cpu",
            DeviceBackend::Cuda => "cuda",
            DeviceBackend::Rocm => "rocm",
            DeviceBackend::Vulkan => "vulkan",
            DeviceBackend::Metal => "metal",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "cpu" => Some(DeviceBackend::Cpu),
            "cuda" => Some(DeviceBackend::Cuda),
            "rocm" => Some(DeviceBackend::Rocm),
            "vulkan" => Some(DeviceBackend::Vulkan),
            "metal" => Some(DeviceBackend::Metal),
            _ => None,
        }
    }

    pub fn is_gpu(&self) -> bool {
        !matches!(self, DeviceBackend::Cpu)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevicePlan {
    pub backend: DeviceBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl DevicePlan {
    pub fn cpu() -> Self {
        DevicePlan {
            backend: DeviceBackend::Cpu,
            ordinal: None,
            key: None,
        }
    }

    pub fn gpu(backend: DeviceBackend, ordinal: u32) -> Self {
        DevicePlan {
            backend,
            ordinal: Some(ordinal),
            key: None,
        }
    }

    pub fn with_key(mut self, key: Option<String>) -> Self {
        self.key = key;
        self
    }

    pub fn learned_key(&self) -> String {
        if let Some(key) = self.key.as_deref() {
            return key.to_string();
        }
        match (self.backend, self.ordinal) {
            (DeviceBackend::Cpu, _) => "cpu".to_string(),
            (backend, Some(n)) => format!("{}:{n}", backend.as_str()),
            (backend, None) => backend.as_str().to_string(),
        }
    }

    pub fn is_gpu(&self) -> bool {
        self.backend.is_gpu()
    }

    pub fn legacy_string(&self) -> String {
        match (self.backend, self.ordinal) {
            (DeviceBackend::Cpu, _) => "cpu".to_string(),
            (backend, None) => backend.as_str().to_string(),
            (backend, Some(0)) => backend.as_str().to_string(),
            (backend, Some(n)) => format!("{}:{n}", backend.as_str()),
        }
    }
}

fn from_legacy_str(raw: &str) -> Option<DevicePlan> {
    let (head, tail) = match raw.split_once(':') {
        Some((head, tail)) => (head, Some(tail)),
        None => (raw, None),
    };
    let backend = DeviceBackend::parse(head)?;
    if backend == DeviceBackend::Cpu {
        return Some(DevicePlan::cpu());
    }
    let ordinal = match tail {
        Some(value) => Some(value.parse::<u32>().ok()?),
        None => Some(0),
    };
    Some(DevicePlan {
        backend,
        ordinal,
        key: None,
    })
}

pub fn plan_device(plan: &serde_json::Value) -> Result<DevicePlan, String> {
    let Some(raw) = plan.get("device") else {
        return Ok(DevicePlan::cpu());
    };
    if let Some(text) = raw.as_str() {
        return from_legacy_str(text)
            .ok_or_else(|| format!("ERR_PLAN: unknown device '{text}' in a stored plan"));
    }
    let device = serde_json::from_value::<DevicePlan>(raw.clone())
        .map_err(|error| format!("ERR_PLAN: unreadable device in a stored plan: {error}"))?;
    if device.key.as_deref() == Some("") {
        return Err("ERR_PLAN: a stored plan's device key must be a non-empty string".into());
    }
    Ok(device)
}

pub fn upcast_plan(plan: &serde_json::Value) -> Result<serde_json::Value, String> {
    let device = plan_device(plan)?;
    let mut next = plan.clone();
    let Some(map) = next.as_object_mut() else {
        return Err("ERR_PLAN: a stored plan must be an object".into());
    };
    map.insert(
        "device".into(),
        serde_json::to_value(device).map_err(|e| e.to_string())?,
    );
    map.insert("plan_revision".into(), serde_json::json!(PLAN_REVISION));
    Ok(next)
}

#[derive(Debug)]
pub struct StoredPlan {
    pub plan: serde_json::Value,
    pub upcast: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanReadError {
    TooNew(u64),
    Unreadable(String),
}

impl std::fmt::Display for PlanReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanReadError::TooNew(revision) => write!(
                f,
                "ERR_PLAN: a stored plan is revision {revision}, newer than {PLAN_REVISION}"
            ),
            PlanReadError::Unreadable(reason) => write!(f, "{reason}"),
        }
    }
}

pub fn read_stored_plan(raw: &str) -> Result<StoredPlan, PlanReadError> {
    let plan: serde_json::Value = serde_json::from_str(raw).map_err(|error| {
        PlanReadError::Unreadable(format!("ERR_PLAN: a stored plan is not json: {error}"))
    })?;
    match plan
        .get("plan_revision")
        .and_then(serde_json::Value::as_u64)
    {
        Some(revision) if revision > PLAN_REVISION => {
            return Err(PlanReadError::TooNew(revision));
        }
        Some(PLAN_REVISION) => {
            plan_device(&plan).map_err(PlanReadError::Unreadable)?;
            return Ok(StoredPlan {
                plan,
                upcast: false,
            });
        }
        _ => {}
    }
    Ok(StoredPlan {
        plan: upcast_plan(&plan).map_err(PlanReadError::Unreadable)?,
        upcast: true,
    })
}

pub fn set_device(plan: &mut serde_json::Value, device: DevicePlan) {
    if let Some(map) = plan.as_object_mut() {
        if let Ok(value) = serde_json::to_value(device) {
            map.insert("device".into(), value);
        }
        map.insert("plan_revision".into(), serde_json::json!(PLAN_REVISION));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_legacy_string_device_upcasts_to_the_typed_shape() {
        for (raw, expected) in [
            ("cpu", DevicePlan::cpu()),
            ("cuda", DevicePlan::gpu(DeviceBackend::Cuda, 0)),
            ("cuda:1", DevicePlan::gpu(DeviceBackend::Cuda, 1)),
            ("vulkan", DevicePlan::gpu(DeviceBackend::Vulkan, 0)),
            ("rocm:2", DevicePlan::gpu(DeviceBackend::Rocm, 2)),
        ] {
            let plan = json!({ "model_id": "m", "device": raw });
            assert_eq!(plan_device(&plan).expect(raw), expected, "{raw}");
        }
    }

    #[test]
    fn the_typed_shape_round_trips() {
        let plan = json!({ "device": { "backend": "cuda", "ordinal": 1 } });
        assert_eq!(
            plan_device(&plan).expect("typed"),
            DevicePlan::gpu(DeviceBackend::Cuda, 1)
        );
    }

    #[test]
    fn an_unknown_backend_is_an_error_not_a_silent_cpu() {
        let plan = json!({ "device": "banana" });
        let error = plan_device(&plan).expect_err("banana is not a device");
        assert!(error.starts_with("ERR_PLAN"), "{error}");

        let typed = json!({ "device": { "backend": "banana" } });
        assert!(plan_device(&typed).is_err());
    }

    #[test]
    fn a_plan_with_no_device_is_cpu() {
        assert_eq!(plan_device(&json!({})).expect("empty"), DevicePlan::cpu());
    }

    #[test]
    fn upcasting_stamps_the_revision_and_keeps_every_other_field() {
        let plan = json!({ "model_id": "m", "device": "cuda:1", "n_gpu_layers": 12 });
        let upcast = upcast_plan(&plan).expect("upcast");
        assert_eq!(upcast["plan_revision"], json!(PLAN_REVISION));
        assert_eq!(upcast["n_gpu_layers"], json!(12));
        assert_eq!(upcast["device"]["backend"], json!("cuda"));
        assert_eq!(upcast["device"]["ordinal"], json!(1));
        assert_eq!(
            plan_device(&upcast).expect("re-read"),
            DevicePlan::gpu(DeviceBackend::Cuda, 1),
            "upcasting has to be idempotent or a resumed job re-upcasts every stage"
        );
    }

    #[test]
    fn a_gpu_plan_is_recognised_through_both_shapes() {
        assert!(plan_device(&json!({ "device": "cuda" }))
            .expect("legacy")
            .is_gpu());
        assert!(
            plan_device(&json!({ "device": { "backend": "vulkan", "ordinal": 0 } }))
                .expect("typed")
                .is_gpu(),
            "a vulkan plan is a GPU plan; reading it as CPU inverts the capability clamp"
        );
        assert!(!plan_device(&json!({ "device": "cpu" }))
            .expect("cpu")
            .is_gpu());
    }

    #[test]
    fn an_empty_device_key_is_err_plan_in_rust_as_it_is_in_python() {
        let plan = json!({ "device": { "backend": "cuda", "ordinal": 0, "key": "" } });
        let error = plan_device(&plan).expect_err("an empty key");
        assert!(
            error.starts_with("ERR_PLAN"),
            "rust accepted a key the engine rejects, so a plan rust wrote fails downstream: \
             {error}"
        );
    }

    #[test]
    fn a_plan_from_a_newer_revision_is_refused_not_downgraded() {
        let raw = serde_json::to_string(&json!({
            "model_id": "m",
            "plan_revision": PLAN_REVISION + 1,
            "device": { "backend": "cuda", "ordinal": 0 }
        }))
        .expect("json");
        let error = read_stored_plan(&raw).expect_err("a newer revision");
        assert_eq!(error, PlanReadError::TooNew(PLAN_REVISION + 1));
        assert!(
            error.to_string().starts_with("ERR_PLAN"),
            "rewriting it as revision {PLAN_REVISION} silently downgrades a plan this binary \
             cannot interpret: {error}"
        );
    }

    #[test]
    fn a_legacy_stored_plan_is_upcast_and_flagged_for_rewrite() {
        let stored = read_stored_plan(r#"{"model_id":"m","device":"cuda:1"}"#).expect("legacy");
        assert!(stored.upcast);
        assert_eq!(stored.plan["plan_revision"], json!(PLAN_REVISION));
        assert_eq!(stored.plan["device"]["ordinal"], json!(1));
    }

    #[test]
    fn a_current_stored_plan_is_returned_untouched() {
        let raw = serde_json::to_string(&json!({
            "model_id": "m",
            "plan_revision": PLAN_REVISION,
            "device": { "backend": "vulkan", "ordinal": 0 }
        }))
        .expect("json");
        let stored = read_stored_plan(&raw).expect("current");
        assert!(
            !stored.upcast,
            "rewriting an already-current row is a wasted write"
        );
        assert_eq!(stored.plan["device"]["backend"], json!("vulkan"));
    }

    #[test]
    fn an_unreadable_stored_plan_is_err_plan_not_a_cpu_plan() {
        for raw in [
            "not json",
            r#"{"device":"banana"}"#,
            r#"{"plan_revision":2,"device":{"backend":"banana"}}"#,
            r#"[1,2,3]"#,
        ] {
            let error = read_stored_plan(raw).expect_err(raw).to_string();
            assert!(error.starts_with("ERR_PLAN"), "{raw}: {error}");
        }
    }

    #[test]
    fn no_reader_outside_this_module_takes_the_device_key_raw() {
        const LAYOUT_ACCELERATOR_CONTRACT: &[&str] = &["tests/layout_stage.rs"];

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders: Vec<String> = Vec::new();
        let mut pending = vec![root.join("src"), root.join("tests")];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(root)
                    .expect("under the manifest")
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative == "src/hardware/device.rs"
                    || LAYOUT_ACCELERATOR_CONTRACT.contains(&relative.as_str())
                {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read file");
                let is_test_file = relative.starts_with("tests/");
                for (number, line, in_test_mod) in classify_lines(&text) {
                    let in_production = !is_test_file && !in_test_mod;
                    let raw_read = line.contains(r#"get("device")"#)
                        || (in_production && line.contains(r#"["device"]"#))
                        || reads_device_as_a_value(&line);
                    if raw_read {
                        offenders.push(format!("{relative}:{number}: {}", line.trim()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the plan device key belongs to hardware::device; a raw reader sees an object, \
             fails to \"not a GPU\" and degrades silently. The layout request carries an \
             unrelated `device` accelerator string and is exempted by name until R-J gives \
             it the same typed shape:\n{}",
            offenders.join("\n")
        );
    }

    fn strip_literals(line: &str) -> String {
        let mut out = String::new();
        let mut in_string = false;
        let mut previous = '\0';
        for ch in line.chars() {
            match ch {
                '"' if previous != '\\' => in_string = !in_string,
                '/' if !in_string && previous == '/' => {
                    out.pop();
                    break;
                }
                _ if !in_string => out.push(ch),
                _ => {}
            }
            previous = ch;
        }
        out
    }

    fn classify_lines(text: &str) -> Vec<(usize, String, bool)> {
        let mut rows = Vec::new();
        let mut depth: i32 = 0;
        let mut saw_cfg_test = false;
        let mut test_mod_depth: Option<i32> = None;
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim_end_matches('\r').to_string();
            let trimmed = line.trim().to_string();
            let bare = strip_literals(&line);
            let opens = bare.matches('{').count() as i32;
            let closes = bare.matches('}').count() as i32;

            if test_mod_depth.is_none() {
                if trimmed.starts_with("#[cfg(test)]") {
                    saw_cfg_test = true;
                } else if saw_cfg_test && trimmed.starts_with("mod ") {
                    test_mod_depth = Some(depth);
                    saw_cfg_test = false;
                } else if !trimmed.is_empty() && !trimmed.starts_with("//") {
                    saw_cfg_test = false;
                }
            }

            rows.push((index + 1, line, test_mod_depth.is_some()));
            depth += opens - closes;
            if let Some(base) = test_mod_depth {
                if depth <= base {
                    test_mod_depth = None;
                }
            }
        }
        rows
    }

    fn reads_device_as_a_value(line: &str) -> bool {
        let needle = r#"["device"]"#;
        let mut from = 0;
        while let Some(at) = line[from..].find(needle) {
            let after = from + at + needle.len();
            if !line[after..].starts_with("[\"") {
                return true;
            }
            from = after;
        }
        false
    }
}
