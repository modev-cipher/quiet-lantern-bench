use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use txtcaster_lib::ingest_fallback::FallbackSource;
use txtcaster_lib::library::{self, ImportEvent};
use txtcaster_lib::orchestrator::stage::{ProgressEvent, ProgressSink};
use txtcaster_lib::orchestrator::state::Scope;
use txtcaster_lib::orchestrator::{
    residency_check, seed_default_voice_profile, CreateJobRequest, JobOrchestrator, JobState,
    JobUpdate, LayoutStage, RenderingStage, ScriptingStage, SidecarProvider, StageRegistry,
    StartOptions, SynthesizingStage, UpdateSink,
};

fn test_options() -> StartOptions {
    StartOptions {
        vram_sampler: residency_check::disabled_sampler(),
        ..StartOptions::default()
    }
}

use txtcaster_lib::storage::{self, book, job, models, now_ms, voice, SqlitePool};

const FAKE_TTS_MODEL: &str = "fake-tts";
const HANDSHAKE_PREFIX: &str = "TXTCASTER_READY";

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .ancestors()
        .find(|dir| dir.join("engine").join("pyproject.toml").is_file())
        .expect("workspace root with engine/")
        .to_path_buf()
}

fn engine_python(root: &Path) -> Option<PathBuf> {
    let venv = if cfg!(windows) {
        root.join("engine")
            .join(".venv")
            .join("Scripts")
            .join("python.exe")
    } else {
        root.join("engine").join(".venv").join("bin").join("python")
    };
    venv.is_file().then_some(venv)
}

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

struct RealEngine {
    child: Child,
    port: u16,
    token: String,
}

impl RealEngine {
    fn spawn(root: &Path, python: &Path, token: &str) -> Self {
        let mut child = Command::new(python)
            .args([
                "-m",
                "txtcaster_engine",
                "--port",
                "0",
                "--token",
                token,
                "--allow-fake-tts",
                "--allow-fake-llm",
            ])
            .current_dir(root.join("engine"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn engine");
        let stdout = child.stdout.take().expect("stdout");
        let mut stderr = child.stderr.take().expect("stderr");
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = stderr.read_to_end(&mut sink);
        });
        let mut reader = BufReader::new(stdout);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut port = None;
        let mut line = String::new();
        while Instant::now() < deadline {
            line.clear();
            let read = reader.read_line(&mut line).expect("read line");
            if read == 0 {
                break;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() == 2 && parts[0] == HANDSHAKE_PREFIX {
                port = parts[1].parse().ok();
                break;
            }
        }
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = reader.read_to_end(&mut sink);
        });
        let port = port.expect("engine handshake");
        RealEngine {
            child,
            port,
            token: token.to_string(),
        }
    }

    async fn wait_healthy(&self) {
        let client = reqwest::Client::new();
        let url = format!("http://127.0.0.1:{}/health", self.port);
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            let ok = client
                .get(&url)
                .bearer_auth(&self.token)
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .map(|resp| resp.status().is_success())
                .unwrap_or(false);
            if ok {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("engine did not become healthy on port {}", self.port);
    }
}

impl Drop for RealEngine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct StaticSidecar {
    port: u16,
    token: String,
}

impl SidecarProvider for StaticSidecar {
    fn connection(&self) -> Option<(u16, String)> {
        Some((self.port, self.token.clone()))
    }
}

fn build_orchestrator(
    pool: SqlitePool,
    engine: &RealEngine,
    models_root: PathBuf,
    dirs: storage::paths::StorageDirs,
    updates: UpdateSink,
) -> JobOrchestrator {
    let sidecar: Arc<dyn SidecarProvider> = Arc::new(StaticSidecar {
        port: engine.port,
        token: engine.token.clone(),
    });
    let cache_dir = dirs.cache_dir.clone();
    let mut registry = StageRegistry::default();
    registry.register(
        JobState::Layout,
        Arc::new(LayoutStage::new(
            sidecar.clone(),
            models_root.clone(),
            cache_dir.clone(),
        )),
    );
    registry.register(
        JobState::Scripting,
        Arc::new(ScriptingStage::new(sidecar.clone(), models_root.clone())),
    );
    registry.register(
        JobState::Synthesizing,
        Arc::new(SynthesizingStage::new(
            sidecar,
            models_root,
            cache_dir.clone(),
        )),
    );
    registry.register(JobState::Rendering, Arc::new(RenderingStage::new(&dirs)));
    let progress: ProgressSink = Arc::new(|_event: ProgressEvent| {});
    JobOrchestrator::start_with_options(
        pool,
        registry,
        progress,
        updates,
        Duration::from_secs(30),
        cache_dir,
        test_options(),
    )
}

fn update_sink() -> (UpdateSink, Arc<Mutex<Vec<JobUpdate>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink_log = log.clone();
    let sink: UpdateSink = Arc::new(move |update| sink_log.lock().unwrap().push(update));
    (sink, log)
}

async fn wait_for_state(pool: &SqlitePool, job_id: &str, target: JobState) -> bool {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Ok(Some(record)) = job::get_job(pool, job_id).await {
            let state = JobState::parse(&record.state).expect("state");
            if state == target {
                return true;
            }
            if matches!(state, JobState::Failed | JobState::Cancelled) && target != state {
                return false;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn wait_for_mid_tts(pool: &SqlitePool, job_id: &str, cache_dir: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Ok(Some(record)) = job::get_job(pool, job_id).await {
            let state = JobState::parse(&record.state).expect("state");
            if state == JobState::Synthesizing && !cache_mtimes(cache_dir).is_empty() {
                return true;
            }
            if matches!(
                state,
                JobState::Done | JobState::Failed | JobState::Cancelled
            ) {
                return false;
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    false
}

fn cache_mtimes(cache_dir: &Path) -> std::collections::BTreeMap<PathBuf, std::time::SystemTime> {
    let mut mtimes = std::collections::BTreeMap::new();
    let Ok(shards) = std::fs::read_dir(cache_dir) else {
        return mtimes;
    };
    for shard in shards.flatten() {
        let Ok(entries) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if let Ok(modified) = meta.modified() {
                    mtimes.insert(entry.path(), modified);
                }
            }
        }
    }
    mtimes
}

async fn wait_cache_quiescent(cache_dir: &Path) {
    let mut previous = cache_mtimes(cache_dir);
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let current = cache_mtimes(cache_dir);
        if current == previous {
            return;
        }
        previous = current;
    }
    panic!("engine cache never went quiescent after kill");
}

async fn install_fake_model(pool: &SqlitePool) {
    let install = models::ModelInstall {
        model_id: FAKE_TTS_MODEL.into(),
        path: "tts/fake-tts".into(),
        sha256: "0".repeat(64),
        disk_bytes: 1,
        source: "test".into(),
        installed_at: now_ms(),
    };
    models::insert_model_install(pool, &install)
        .await
        .expect("install fake model");
}

async fn seed_fake_voice(pool: &SqlitePool) -> String {
    let profile = voice::VoiceProfile {
        id: storage::new_id(),
        name: "Fake TTS".into(),
        engine: FAKE_TTS_MODEL.into(),
        kind: "preset".into(),
        preset_voice_id: Some("af_test".into()),
        reference_wav_path: None,
        params_json: None,
        created_at: now_ms(),
    };
    voice::insert_voice_profile(pool, &profile)
        .await
        .expect("voice");
    profile.id
}

struct Harness {
    _dir: tempfile::TempDir,
    pool: SqlitePool,
    engine: RealEngine,
    python: PathBuf,
    root: PathBuf,
    fixture: PathBuf,
    models_root: PathBuf,
    cache_dir: PathBuf,
}

impl Harness {
    async fn kill_and_respawn_engine(&mut self) {
        let token = self.engine.token.clone();
        self.engine = RealEngine::spawn(&self.root, &self.python, &token);
        self.engine.wait_healthy().await;
    }
}

async fn setup() -> Option<Harness> {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not on PATH");
        return None;
    }
    let root = workspace_root();
    let Some(python) = engine_python(&root) else {
        eprintln!("skipping: engine venv not found (run `uv sync` in engine/)");
        return None;
    };
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = dir.path().join("fixture.pdf");
    let status = Command::new(&python)
        .arg(root.join("scripts").join("make_fixture_pdf.py"))
        .arg(&fixture)
        .args(["--chapters", "60", "--pages-per-chapter", "5"])
        .status()
        .expect("run fixture script");
    assert!(status.success(), "fixture generation failed");

    let pool = storage::open_pool(&dir.path().join("db.sqlite"))
        .await
        .expect("open pool");
    seed_default_voice_profile(&pool).await.expect("seed voice");
    install_fake_model(&pool).await;

    let token = "e2e-test-token";
    let engine = RealEngine::spawn(&root, &python, token);
    engine.wait_healthy().await;
    Some(Harness {
        _dir: dir,
        pool,
        engine,
        python,
        root,
        fixture,
        models_root: PathBuf::new(),
        cache_dir: PathBuf::new(),
    })
}

fn make_fixture_epub(h: &Harness) -> PathBuf {
    let fixture = h._dir.path().join("fixture.epub");
    let status = Command::new(&h.python)
        .arg(h.root.join("scripts").join("make_fixture_epub.py"))
        .arg(&fixture)
        .args(["--chapters", "3", "--paragraphs-per-chapter", "12"])
        .status()
        .expect("run epub fixture script");
    assert!(status.success(), "epub fixture generation failed");
    fixture
}

async fn import_epub_fixture(h: &Harness) -> String {
    let client = reqwest::Client::new();
    let books_dir = h._dir.path().join("books");
    let fixture = make_fixture_epub(h);
    let summary = library::import_book(
        &h.pool,
        &client,
        h.engine.port,
        &h.engine.token,
        &books_dir,
        &fixture,
        None,
        true,
        &FallbackSource::off(),
        |_event: ImportEvent| {},
    )
    .await
    .expect("import epub fixture")
    .book;
    assert_eq!(summary.title, "Fixture EPUB Title");
    assert!(
        summary.page_count.unwrap_or(0) > 3,
        "reflow produced too few pages"
    );
    summary.id
}

async fn import_fixture(h: &Harness) -> String {
    let client = reqwest::Client::new();
    let books_dir = h._dir.path().join("books");
    let summary = library::import_book(
        &h.pool,
        &client,
        h.engine.port,
        &h.engine.token,
        &books_dir,
        &h.fixture,
        None,
        true,
        &FallbackSource::off(),
        |_event: ImportEvent| {},
    )
    .await
    .expect("import fixture")
    .book;
    assert_eq!(summary.page_count, Some(300));
    summary.id
}

async fn two_chapter_scope(pool: &SqlitePool, book_id: &str) -> Scope {
    let chapters = book::list_chapters(pool, book_id).await.expect("chapters");
    assert_eq!(chapters.len(), 60);
    Scope::Chapters {
        chapter_ids: vec![chapters[0].id.clone(), chapters[1].id.clone()],
    }
}

fn make_request(book_id: &str, scope: Scope, voice_id: &str) -> CreateJobRequest {
    CreateJobRequest {
        book_id: book_id.into(),
        scope,
        mode: "narrator".into(),
        voice_profile_id: voice_id.into(),
        output_format: "mp3".into(),
        model_overrides_json: None,
        overnight: false,
        verify: false,
    }
}

fn dirs(h: &mut Harness) -> (PathBuf, PathBuf, storage::paths::StorageDirs) {
    h.models_root = h._dir.path().join("models");
    h.cache_dir = h._dir.path().join("cache");
    (
        h.models_root.clone(),
        h.cache_dir.clone(),
        storage::paths::defaults(h._dir.path()),
    )
}

#[tokio::test]
#[ignore = "requires engine venv and ffmpeg; run in CI mvp-e2e job"]
async fn mvp_pdf_to_mp3_end_to_end() {
    let Some(mut h) = setup().await else {
        return;
    };
    let voice_id = seed_fake_voice(&h.pool).await;
    let book_id = import_fixture(&h).await;
    let scope = two_chapter_scope(&h.pool, &book_id).await;
    let (models_root, _cache_dir, storage_dirs) = dirs(&mut h);

    let (updates, _log) = update_sink();
    let orchestrator = build_orchestrator(
        h.pool.clone(),
        &h.engine,
        models_root,
        storage_dirs,
        updates,
    );
    let job_id = orchestrator
        .create_job(make_request(&book_id, scope, &voice_id))
        .await
        .expect("create job");

    assert!(
        wait_for_state(&h.pool, &job_id, JobState::Done).await,
        "job did not reach Done"
    );

    let record = job::get_job(&h.pool, &job_id).await.unwrap().unwrap();
    let output_paths: Vec<String> =
        serde_json::from_str(record.output_paths_json.as_deref().unwrap()).unwrap();
    assert_eq!(output_paths.len(), 1);
    let output = Path::new(&output_paths[0]);
    assert!(output.is_file(), "mp3 not written");
    assert_eq!(output.extension().unwrap(), "mp3");

    let chapters_json = ffprobe(&[
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_chapters",
        &output_paths[0],
    ]);
    let parsed: serde_json::Value = serde_json::from_str(&chapters_json).unwrap();
    assert_eq!(parsed["chapters"].as_array().unwrap().len(), 2);

    let segments = job::list_segments(&h.pool, &job_id).await.unwrap();
    assert!(!segments.is_empty(), "no segments produced");
    assert!(
        segments.iter().all(|s| s.audio_hash.is_some()),
        "some segments were not synthesized"
    );
}

#[tokio::test]
#[ignore = "requires engine venv and ffmpeg; run in CI mvp-e2e job"]
async fn mvp_epub_to_mp3_end_to_end() {
    let Some(mut h) = setup().await else {
        return;
    };
    let voice_id = seed_fake_voice(&h.pool).await;
    let book_id = import_epub_fixture(&h).await;

    let chapters = book::list_chapters(&h.pool, &book_id)
        .await
        .expect("chapters");
    assert_eq!(
        chapters
            .iter()
            .map(|c| c.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Chapter 1", "Chapter 2", "Chapter 3"]
    );
    assert!(chapters.iter().all(|c| !c.is_pseudo));
    assert_eq!(chapters[0].page_start, 1);
    for pair in chapters.windows(2) {
        assert_eq!(pair[1].page_start, pair[0].page_end + 1);
    }

    let scope = Scope::Chapters {
        chapter_ids: vec![chapters[0].id.clone(), chapters[1].id.clone()],
    };
    let (models_root, _cache_dir, storage_dirs) = dirs(&mut h);
    let (updates, _log) = update_sink();
    let orchestrator = build_orchestrator(
        h.pool.clone(),
        &h.engine,
        models_root,
        storage_dirs,
        updates,
    );
    let job_id = orchestrator
        .create_job(make_request(&book_id, scope, &voice_id))
        .await
        .expect("create job");

    assert!(
        wait_for_state(&h.pool, &job_id, JobState::Done).await,
        "epub job did not reach Done"
    );

    let record = job::get_job(&h.pool, &job_id).await.unwrap().unwrap();
    let output_paths: Vec<String> =
        serde_json::from_str(record.output_paths_json.as_deref().unwrap()).unwrap();
    assert_eq!(output_paths.len(), 1);
    let output = Path::new(&output_paths[0]);
    assert!(output.is_file(), "mp3 not written");
    assert_eq!(output.extension().unwrap(), "mp3");

    let chapters_json = ffprobe(&[
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_chapters",
        &output_paths[0],
    ]);
    let parsed: serde_json::Value = serde_json::from_str(&chapters_json).unwrap();
    assert_eq!(parsed["chapters"].as_array().unwrap().len(), 2);
}

#[tokio::test]
#[ignore = "requires engine venv and ffmpeg; run in CI mvp-e2e job"]
async fn mvp_resume_after_kill_redoes_nothing() {
    let Some(mut h) = setup().await else {
        return;
    };
    let voice_id = seed_fake_voice(&h.pool).await;
    let book_id = import_fixture(&h).await;
    let scope = two_chapter_scope(&h.pool, &book_id).await;
    let (models_root, cache_dir, storage_dirs) = dirs(&mut h);

    let (updates, _log) = update_sink();
    let orchestrator = build_orchestrator(
        h.pool.clone(),
        &h.engine,
        models_root.clone(),
        storage_dirs.clone(),
        updates,
    );
    let job_id = orchestrator
        .create_job(make_request(&book_id, scope, &voice_id))
        .await
        .expect("create job");
    assert!(
        wait_for_mid_tts(&h.pool, &job_id, &cache_dir).await,
        "job never reached mid-synthesis"
    );

    drop(orchestrator);
    h.kill_and_respawn_engine().await;
    wait_cache_quiescent(&cache_dir).await;

    let record = job::get_job(&h.pool, &job_id).await.unwrap().unwrap();
    assert_eq!(
        JobState::parse(&record.state).unwrap(),
        JobState::Synthesizing,
        "job should be frozen mid-TTS after kill"
    );
    let segments_at_kill = job::list_segments(&h.pool, &job_id).await.unwrap();
    let completed_at_kill: Vec<(String, String)> = segments_at_kill
        .iter()
        .filter_map(|s| s.audio_hash.clone().map(|hash| (s.id.clone(), hash)))
        .collect();
    let mtimes_at_kill = cache_mtimes(&cache_dir);

    let (updates2, _log2) = update_sink();
    let orchestrator2 = build_orchestrator(
        h.pool.clone(),
        &h.engine,
        models_root,
        storage_dirs,
        updates2,
    );
    let resumed = orchestrator2.resume_on_launch().await.expect("resume");
    assert!(resumed.contains(&job_id), "job was not re-queued on launch");
    let reached = wait_for_state(&h.pool, &job_id, JobState::Done).await;
    if !reached {
        let dbg = job::get_job(&h.pool, &job_id).await.unwrap().unwrap();
        panic!("resumed job did not reach Done, final state: {}", dbg.state);
    }

    let segments_after = job::list_segments(&h.pool, &job_id).await.unwrap();
    assert!(segments_after.iter().all(|s| s.audio_hash.is_some()));
    for (segment_id, hash_at_kill) in &completed_at_kill {
        let after = segments_after
            .iter()
            .find(|s| &s.id == segment_id)
            .expect("segment survived resume");
        assert_eq!(
            after.audio_hash.as_deref(),
            Some(hash_at_kill.as_str()),
            "completed segment changed audio hash after resume"
        );
    }
    let mtimes_after = cache_mtimes(&cache_dir);
    for (path, mtime_at_kill) in &mtimes_at_kill {
        assert_eq!(
            mtimes_after.get(path),
            Some(mtime_at_kill),
            "cached wav {} was rewritten during resume",
            path.display()
        );
    }
    let record = job::get_job(&h.pool, &job_id).await.unwrap().unwrap();
    assert!(record.output_paths_json.is_some(), "no MP3 after resume");
}

#[tokio::test]
#[ignore = "requires engine venv and ffmpeg; run in CI mvp-e2e job"]
async fn mvp_second_run_hits_cache() {
    let Some(mut h) = setup().await else {
        return;
    };
    let voice_id = seed_fake_voice(&h.pool).await;
    let book_id = import_fixture(&h).await;
    let scope = two_chapter_scope(&h.pool, &book_id).await;
    let (models_root, cache_dir, storage_dirs) = dirs(&mut h);

    let (updates, _log) = update_sink();
    let orchestrator = build_orchestrator(
        h.pool.clone(),
        &h.engine,
        models_root.clone(),
        storage_dirs,
        updates,
    );
    let scope2 = scope.clone();
    let first = Instant::now();
    let job_id = orchestrator
        .create_job(make_request(&book_id, scope, &voice_id))
        .await
        .expect("create job");
    assert!(wait_for_state(&h.pool, &job_id, JobState::Done).await);
    let first_elapsed = first.elapsed();
    let first_hashes: std::collections::BTreeSet<String> = job::list_segments(&h.pool, &job_id)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|s| s.audio_hash)
        .collect();
    let mtimes_before = cache_mtimes(&cache_dir);

    let second = Instant::now();
    let job_id2 = orchestrator
        .create_job(make_request(&book_id, scope2, &voice_id))
        .await
        .expect("create job 2");
    assert!(wait_for_state(&h.pool, &job_id2, JobState::Done).await);
    let second_elapsed = second.elapsed();

    let segments = job::list_segments(&h.pool, &job_id2).await.unwrap();
    let total = segments.len();
    assert!(total > 0);
    assert!(
        segments.iter().all(|s| s.audio_hash.is_some()),
        "second run left segments unsynthesized"
    );

    let cached = segments
        .iter()
        .filter(|s| {
            s.audio_hash
                .as_deref()
                .map(|hash| first_hashes.contains(hash))
                .unwrap_or(false)
        })
        .count();
    let ratio = cached as f64 / total as f64;
    assert!(
        ratio >= 0.95,
        "cache hit ratio {ratio} below 0.95 ({cached}/{total})"
    );
    let mtimes_after = cache_mtimes(&cache_dir);
    for (path, mtime_before) in &mtimes_before {
        assert_eq!(
            mtimes_after.get(path),
            Some(mtime_before),
            "cached wav {} was re-synthesized on second run",
            path.display()
        );
    }
    eprintln!(
        "cache-hit run: first {first_elapsed:?}, second {second_elapsed:?}, hits {cached}/{total}"
    );
}

const FAKE_LLM_MODEL: &str = "gemma4-e4b-q4km";
const FAKE_LLM_GGUF: &str = "gemma-4-E4B-it-Q4_K_M.gguf";
const FAKE_GGUF_MAGIC: &[u8] = b"FAKEGGUF";

const TIERS: [(&str, &str, u32); 3] = [
    ("full-gpu", "cuda", 32),
    ("hybrid", "cuda", 16),
    ("cpu", "cpu", 0),
];

async fn install_fake_llm(pool: &SqlitePool, models_root: &Path) {
    let dir = models_root.join("llm").join(FAKE_LLM_MODEL);
    std::fs::create_dir_all(&dir).expect("create fake llm dir");
    std::fs::write(dir.join(FAKE_LLM_GGUF), FAKE_GGUF_MAGIC).expect("write fake gguf");
    let install = models::ModelInstall {
        model_id: FAKE_LLM_MODEL.into(),
        path: format!("llm/{FAKE_LLM_MODEL}"),
        sha256: "0".repeat(64),
        disk_bytes: FAKE_GGUF_MAGIC.len() as i64,
        source: "test".into(),
        installed_at: now_ms(),
    };
    models::insert_model_install(pool, &install)
        .await
        .expect("install fake llm");
}

fn tier_plan(device: &str, n_gpu_layers: u32) -> serde_json::Value {
    serde_json::json!({
        "model_id": FAKE_LLM_MODEL,
        "device": device,
        "n_gpu_layers": n_gpu_layers,
        "n_layers_total": 32,
        "kv_offload": true,
        "ctx": 4096,
        "batch": 256,
    })
}

fn segment_flags(segment: &job::ScriptSegment) -> Vec<String> {
    segment
        .flags_json
        .as_deref()
        .map(|raw| serde_json::from_str::<Vec<String>>(raw).expect("flags_json is a string array"))
        .unwrap_or_default()
}

async fn insert_teacher_job(
    pool: &SqlitePool,
    book_id: &str,
    voice_id: &str,
    plan: &serde_json::Value,
) -> String {
    let now = now_ms();
    let job_id = storage::new_id();
    let record = job::RenderJob {
        id: job_id.clone(),
        book_id: book_id.to_string(),
        scope_json: serde_json::to_string(&Scope::Pages {
            page_start: 1,
            page_end: 3,
        })
        .unwrap(),
        mode: "teacher".into(),
        voice_profile_id: voice_id.to_string(),
        output_format: "mp3".into(),
        model_overrides_json: Some(format!(r#"{{"llm": "{FAKE_LLM_MODEL}"}}"#)),
        resource_plan_json: Some(plan.to_string()),
        vision_plan_json: None,
        tts_plan_json: None,
        align_plan_json: None,
        state: JobState::Queued.as_str().to_string(),
        checkpoint_json: None,
        overnight: false,
        eta_seconds: None,
        report_json: None,
        output_paths_json: None,
        tts_seed: None,
        verify: false,
        created_at: now,
        updated_at: now,
    };
    job::insert_job(pool, &record).await.expect("insert job");
    job_id
}

#[tokio::test]
#[ignore = "requires engine venv and ffmpeg; run in CI mvp-e2e job"]
async fn phase2_teacher_three_tier_matrix_same_model() {
    let Some(mut h) = setup().await else {
        return;
    };
    let voice_id = seed_fake_voice(&h.pool).await;
    let book_id = import_fixture(&h).await;
    let (models_root, _cache_dir, storage_dirs) = dirs(&mut h);
    install_fake_llm(&h.pool, &models_root).await;

    let (updates, _log) = update_sink();
    let orchestrator = build_orchestrator(
        h.pool.clone(),
        &h.engine,
        models_root,
        storage_dirs,
        updates,
    );

    let mut jobs = Vec::new();
    for (tier, device, n_gpu_layers) in TIERS {
        let plan = tier_plan(device, n_gpu_layers);
        let job_id = insert_teacher_job(&h.pool, &book_id, &voice_id, &plan).await;
        jobs.push((tier, device, n_gpu_layers, job_id));
    }
    let resumed = orchestrator.resume_on_launch().await.expect("resume");
    for (_, _, _, job_id) in &jobs {
        assert!(resumed.contains(job_id), "tier job was not queued");
    }

    for (tier, device, n_gpu_layers, job_id) in &jobs {
        assert!(
            wait_for_state(&h.pool, job_id, JobState::Done).await,
            "{tier} tier job did not reach Done"
        );
        let record = job::get_job(&h.pool, job_id).await.unwrap().unwrap();
        let plan: serde_json::Value =
            serde_json::from_str(record.resource_plan_json.as_deref().unwrap()).unwrap();
        let stored_device = txtcaster_lib::hardware::device::plan_device(&plan)
            .expect("a stored plan names a readable device");
        assert_eq!(
            stored_device.legacy_string(),
            *device,
            "{tier} plan device changed"
        );
        assert_eq!(
            plan["n_gpu_layers"], *n_gpu_layers,
            "{tier} plan layers changed"
        );

        let segments = job::list_segments(&h.pool, job_id).await.unwrap();
        assert!(!segments.is_empty(), "{tier} produced no segments");
        assert!(
            segments.iter().all(|s| s.audio_hash.is_some()),
            "{tier} left segments unsynthesized"
        );
        let echo = format!(
            "device {} with {n_gpu_layers} of 32 gpu layers",
            stored_device.learned_key()
        );
        assert!(
            segments.iter().any(|s| s.text.contains(&echo)),
            "{tier} lecture text missing plan echo '{echo}'"
        );
        let fallback = segments
            .iter()
            .any(|s| segment_flags(s).iter().any(|f| f == "fallback_narrator"));
        assert!(!fallback, "{tier} fell back to narrator");

        let output_paths: Vec<String> =
            serde_json::from_str(record.output_paths_json.as_deref().unwrap()).unwrap();
        assert_eq!(output_paths.len(), 1, "{tier} produced no MP3");
        assert!(
            Path::new(&output_paths[0]).is_file(),
            "{tier} MP3 not written"
        );
    }
}

#[tokio::test]
#[ignore = "requires engine venv and ffmpeg; run in CI mvp-e2e job"]
async fn phase2_narrator_llm_normalization_end_to_end() {
    let Some(mut h) = setup().await else {
        return;
    };
    let voice_id = seed_fake_voice(&h.pool).await;
    let book_id = import_fixture(&h).await;
    let (models_root, _cache_dir, storage_dirs) = dirs(&mut h);
    install_fake_llm(&h.pool, &models_root).await;
    storage::settings::set_setting(&h.pool, "narrator.llm_normalize", "true")
        .await
        .expect("opt into llm normalization");

    let (updates, _log) = update_sink();
    let orchestrator = build_orchestrator(
        h.pool.clone(),
        &h.engine,
        models_root,
        storage_dirs,
        updates,
    );

    let scope = Scope::Pages {
        page_start: 1,
        page_end: 3,
    };
    let mut request = make_request(&book_id, scope, &voice_id);
    request.model_overrides_json = Some(format!(r#"{{"llm": "{FAKE_LLM_MODEL}"}}"#));
    let job_id = orchestrator.create_job(request).await.expect("create job");

    assert!(
        wait_for_state(&h.pool, &job_id, JobState::Done).await,
        "narrator llm job did not reach Done"
    );
    let segments = job::list_segments(&h.pool, &job_id).await.unwrap();
    assert!(!segments.is_empty());
    let normalized = segments
        .iter()
        .any(|s| segment_flags(s).iter().any(|f| f == "llm_normalized"));
    assert!(normalized, "no segment carries llm_normalized flag");
}

fn ffprobe(args: &[&str]) -> String {
    let out = Command::new("ffprobe")
        .args(args)
        .output()
        .expect("ffprobe");
    assert!(out.status.success(), "ffprobe failed");
    String::from_utf8_lossy(&out.stdout).to_string()
}
