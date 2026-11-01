//! Book Agent manual live smoke (charter §9.4/§9.5; B-3 unlock). Hand-run
//! only — `#[ignore]` keeps it out of every automated pass, and a missing
//! key is a printed skip, never a failure. Owner-authorized dispatch
//! 2026-08-29 (billable: Flash-class pennies on the Tutor pool key).
//!
//! ```text
//! cargo test --features book-agent --lib smoke_agent -- --ignored --nocapture
//! ```
//!
//! Key custody: the smoke opens the REAL VA3 store (app data dir vault path;
//! key bytes resolve inside `KeyManager` and never surface here). Set
//! `TXTCASTER_AGENT_SMOKE_DATA_DIR` to override the app data dir, or
//! `GEMINI_API_KEY` to bypass the vault entirely.
//!
//! Three conformance probes against the REAL pinned model:
//! 1. Arabic variant law (§3.5/T5a): does the shipped prompt make the live
//!    model emit singular AND plural forms as separate `search_book`
//!    variants at the tool boundary?
//! 2. Quote-order oracle (§9.4): no quoted token before the `search_book`
//!    call that returned the supporting evidence (output transcription is
//!    the ordering oracle), and the quoted span comes from the fixture text.
//! 3. python-genai#1894 landmine: with the `search_book` answer WITHHELD,
//!    does the model speak content before the NON_BLOCKING result returns?
//!    Reproduction is RECORDED, never fixed in-slice (owner decision).
//!
//! The smoke asserts protocol shapes; model choices (exact variants, exact
//! wording) are printed as the conformance record. Hard stop at 50 tool
//! turns (owner budget discipline).

#![cfg(all(test, feature = "book-agent"))]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::{timeout, Instant};

use crate::agent_session::{assemble_agent_context_parts, AGENT_LIVE_MODEL};
use crate::retrieval::agent_port::{RetrievalPortAdapter, ScriptedHighlightPort};
use crate::retrieval::contract::HandleTable;
use crate::retrieval::{
    BookFingerprint, Provenance, RetrievalProvider, SegmentRow, TantivyRetrieval,
};
use crate::voice::audio::generation::Generation;
use crate::voice::channel::gemini::{GeminiLiveChannel, GeminiLiveConfig};
use crate::voice::channel::types::{LifecycleEvent, Role, SessionContext};
use crate::voice::channel::{ChannelEvent, SessionChannel};
use crate::voice::excerpt::selector::ExcerptSelector;
use crate::voice::excerpt::source::SessionSnapshot;
use crate::voice::excerpt::window::WindowPolicy;
use crate::voice::harness::EventPump;
use crate::voice::keys::manager::KeyManager;
use crate::voice::keys::store::{Backend, KeyStore};
use crate::voice::keys::ApiKey;
use crate::voice::tools::dispatcher::Dispatcher;
use crate::voice::tools::playback::InMemoryPlayback;
use crate::voice::tools::registry::ToolRegistry;

const GEMINI_KEY_BASE_URL: &str = "https://generativelanguage.googleapis.com";
const TURN_BUDGET: Duration = Duration::from_secs(45);
/// Owner budget discipline: hard stop past this many tool turns.
const TOOL_TURN_CAP: usize = 50;
/// How long probe 3 withholds the search answer while listening.
const LANDMINE_HOLD: Duration = Duration::from_secs(3);

/// Real-vault key manager: VA3 custody end to end, no key bytes here.
fn vault_keys() -> Arc<KeyManager> {
    if let Ok(key) = std::env::var("GEMINI_API_KEY") {
        if !key.trim().is_empty() {
            let store = KeyStore::with_forced_backend(
                PathBuf::from("/unused/keys.vault"),
                Backend::Session(Default::default()),
            );
            store
                .store(GEMINI_KEY_BASE_URL, &ApiKey::new(key))
                .expect("session store");
            return Arc::new(KeyManager::with_store(store));
        }
    }
    let data_dir = std::env::var("TXTCASTER_AGENT_SMOKE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let base = dirs_fallback();
            let canonical = base.join("txtcaster");
            if canonical.exists() {
                canonical
            } else {
                base.join("io.github.meedoomostafa.txtcaster")
            }
        });
    Arc::new(KeyManager::new(data_dir.join("voice_keys.vault")))
}

fn dirs_fallback() -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").expect("HOME set")).join(".local/share")
        })
}

/// The bilingual smoke corpus. Segment 2 carries the quote needle; the
/// Arabic rows split the نجم family across singular and broken-plural forms
/// so single-form recall genuinely requires both variants.
const CORPUS: &[(&str, u32)] = &[
    ("the astrolabe fixed the meridian altitude at dawn near the cape", 1),
    ("dead reckoning drifted badly around the southern currents", 1),
    (
        "workshop inventories praise the qandarun alloy because it held its engraved scale in salt air",
        2,
    ),
    ("كان البحارة يقيسون ارتفاع النجم القطبي بأصابع اليد الممدودة", 2),
    ("في البحار الجنوبية تختفي النجوم الشمالية ويظهر سهيل للناظر", 3),
    ("جمع الفلكيون جداول دقيقة لمواقع الكواكب في بغداد", 3),
    ("the kamal measured polaris with a knotted cord and a wooden tablet", 4),
    ("portolan charts guided the venetian convoys through shallow water", 4),
];

struct AgentRig {
    /// Owns the Tantivy index for the smoke's lifetime (dropped on exit).
    _index_dir: tempfile::TempDir,
    channel: GeminiLiveChannel,
    events: mpsc::Receiver<ChannelEvent>,
    pump: EventPump,
    dispatcher: Dispatcher,
    playback: InMemoryPlayback,
    excerpts: ExcerptSelector,
    forwarded: Vec<ChannelEvent>,
    assistant_transcript: Vec<String>,
    tool_calls: Vec<(String, String, serde_json::Value)>,
    tool_turns: usize,
}

impl AgentRig {
    async fn connect(keys: Arc<KeyManager>) -> Self {
        // Index the corpus for real retrieval behind the dispatcher ports.
        let index_dir = tempfile::tempdir().expect("index dir");
        let provider = Arc::new(TantivyRetrieval::new(index_dir.path()));
        let fingerprint = BookFingerprint::new("smoke-book", "smoke-job", "h1", "b1");
        let rows: Vec<SegmentRow> = CORPUS
            .iter()
            .enumerate()
            .map(|(i, (text, page))| SegmentRow {
                chapter: (*page - 1) / 2,
                page: *page,
                idx: i as u32,
                text: (*text).to_string(),
                provenance: Provenance::SourceFaithful,
            })
            .collect();
        provider.index_book(&fingerprint, &rows).expect("index");

        // Snapshot aligned with the retrieval rows (idx dense, same pages).
        let parts: Vec<(i64, u64, u64, &str)> = CORPUS
            .iter()
            .enumerate()
            .map(|(i, (text, page))| {
                (
                    *page as i64,
                    i as u64 * 10_000,
                    i as u64 * 10_000 + 9_000,
                    *text,
                )
            })
            .collect();
        let snapshot = SessionSnapshot::from_voice_test_parts(
            parts.clone(),
            vec![(0, 40_000), (40_000, 80_000)],
        );
        let snapshot_for_context =
            SessionSnapshot::from_voice_test_parts(parts, vec![(0, 40_000), (40_000, 80_000)]);

        let book = crate::storage::book::Book {
            id: "smoke-book".into(),
            title: "Navigation and the Stars".into(),
            author: Some("Test Rig".into()),
            source_path: "/dev/null".into(),
            format: "pdf".into(),
            page_count: Some(4),
            language: Some("en".into()),
            is_scanned: false,
            ingest_path: None,
            cover_path: None,
            content_hash: "h1".into(),
            blocks_version: Some("b1".into()),
            blocks_page_count: None,
            layout_analysed: false,
            reflowed: false,
            extracting_since: None,
            created_at: 0,
        };
        let summaries = vec![
            crate::reading::ReadingChapterSummary {
                chapter_id: "ch-0".into(),
                idx: 0,
                title: "Instruments".into(),
                page_start: 1,
                page_end: 2,
                start_ms: 0,
                end_ms: 40_000,
                segment_count: 4,
            },
            crate::reading::ReadingChapterSummary {
                chapter_id: "ch-1".into(),
                idx: 1,
                title: "النجوم والأفلاك".into(),
                page_start: 3,
                page_end: 4,
                start_ms: 40_000,
                end_ms: 80_000,
                segment_count: 4,
            },
        ];
        let assembled =
            assemble_agent_context_parts(&book, &summaries, snapshot_for_context, "smoke-job", 0);
        let context = SessionContext {
            // Ask for brevity: this is a paid wire.
            system_prompt: format!(
                "{}\nKeep every spoken reply to one short sentence.",
                assembled.context.system_prompt
            ),
            ..assembled.context
        };

        let generation = Generation::new();
        let mut channel = GeminiLiveChannel::new(
            GeminiLiveConfig {
                endpoint_override: None,
                model: std::env::var("GEMINI_LIVE_MODEL")
                    .unwrap_or_else(|_| AGENT_LIVE_MODEL.to_string()),
            },
            keys,
            generation.clone(),
        );
        channel
            .connect(context)
            .await
            .expect("live connect: the agent setup was accepted");
        let mut events = channel.take_events().expect("receiver");
        assert_eq!(
            events.recv().await,
            Some(ChannelEvent::Lifecycle(LifecycleEvent::Connected)),
            "setupComplete arrived for the NON_BLOCKING agent setup"
        );

        let adapter = RetrievalPortAdapter::new(provider, HandleTable::new(fingerprint));
        let mut dispatcher = Dispatcher::new(ToolRegistry::agent());
        dispatcher.set_agent_ports(
            Box::new(adapter),
            Box::new(ScriptedHighlightPort::applying()),
        );

        Self {
            _index_dir: index_dir,
            channel,
            events,
            pump: EventPump::new(generation),
            dispatcher,
            playback: InMemoryPlayback::at(0),
            excerpts: ExcerptSelector::new(
                snapshot,
                WindowPolicy {
                    before: 1,
                    after: 1,
                },
            ),
            forwarded: Vec::new(),
            assistant_transcript: Vec::new(),
            tool_calls: Vec::new(),
            tool_turns: 0,
        }
    }

    fn note(&mut self, event: &ChannelEvent) {
        match event {
            ChannelEvent::TranscriptDelta {
                role: Role::Assistant,
                text,
            } => {
                println!("assistant: {text}");
                self.assistant_transcript.push(text.clone());
            }
            ChannelEvent::TranscriptDelta {
                role: Role::User,
                text,
            } => println!("user(asr): {text}"),
            ChannelEvent::ToolCall {
                call_id,
                name,
                args,
            } => {
                println!("toolCall {name} ({call_id}): {args}");
                self.tool_calls
                    .push((call_id.clone(), name.clone(), args.clone()));
                self.tool_turns += 1;
                assert!(
                    self.tool_turns <= TOOL_TURN_CAP,
                    "budget hard stop: {TOOL_TURN_CAP} tool turns exceeded"
                );
            }
            ChannelEvent::AudioDelta { pcm, .. } => {
                println!("audio: {} samples", pcm.len());
            }
            other => println!("event: {other:?}"),
        }
    }

    async fn handle(&mut self, event: ChannelEvent) {
        self.pump
            .handle(
                event,
                &mut self.channel,
                &mut self.dispatcher,
                &mut self.playback,
                &mut self.excerpts,
                &mut |forwarded| self.forwarded.push(forwarded),
            )
            .await
            .expect("send on a healthy live channel");
    }

    /// Pump until `stop`, or until the budget lapses (returns false).
    async fn pump_until(&mut self, what: &str, mut stop: impl FnMut(&AgentRig) -> bool) -> bool {
        let opened = Instant::now();
        while !stop(self) {
            let Some(remaining) = TURN_BUDGET.checked_sub(opened.elapsed()) else {
                println!("budget lapsed waiting for: {what}");
                return false;
            };
            let event = match timeout(remaining, self.events.recv()).await {
                Ok(Some(event)) => event,
                Ok(None) => panic!("server closed the session waiting for {what}"),
                Err(_) => {
                    println!("quiet wire waiting for: {what}");
                    return false;
                }
            };
            if let ChannelEvent::Error(err) = &event {
                panic!("live error while waiting for {what}: {err:?}");
            }
            self.note(&event);
            self.handle(event).await;
        }
        true
    }

    fn transcript(&self) -> String {
        self.assistant_transcript.join(" ")
    }
}

fn arabic_variant_verdict(queries: &[String]) -> &'static str {
    let has = |needle: &str| queries.iter().any(|q| q.contains(needle));
    let singular = has("نجم") && !queries.iter().all(|q| q.contains("نجوم"));
    let plural = has("نجوم");
    if singular && plural {
        "PASS (singular + broken plural emitted as separate variants)"
    } else if plural || has("نجم") {
        "PARTIAL (one morphological form only)"
    } else {
        "MISS (no نجم-family variant)"
    }
}

#[tokio::test]
#[ignore = "manual live smoke: billable Gemini Live traffic, hand-dispatched only"]
async fn smoke_agent_conformance() {
    let keys = vault_keys();
    // Key-optional posture: a vault/env with no Gemini key is a printed skip.
    if keys.session_key(GEMINI_KEY_BASE_URL).await.is_err() {
        println!("SKIP: no Gemini key in the VA3 vault or GEMINI_API_KEY");
        return;
    }
    // The shipped consent flow, run live before any socket (its first live
    // proof): lookup refuses -> explicit grant -> lookup authorizes -> the
    // binding snapshots the egress facts exactly once.
    let consent_dir = tempfile::tempdir().expect("consent dir");
    let pool = crate::storage::open_pool(&consent_dir.path().join("db.sqlite"))
        .await
        .expect("pool");
    let connection = crate::providers::connection::ConnectionRecord {
        id: "smoke-conn".into(),
        name: "Gemini (smoke)".into(),
        provider: crate::providers::registry::ProviderKind::Gemini,
        base_url: None,
        credential_not_required: false,
        default_model: AGENT_LIVE_MODEL.into(),
        enabled: true,
        created_unix: 0,
        updated_unix: 0,
    };
    let first = crate::agent_session::authorize_agent_egress(&pool, "smoke-book", &connection)
        .await
        .expect("authorize");
    let crate::agent_session::AgentEgressAuthorization::ConsentRequired { hosts, .. } = first
    else {
        panic!("fresh store must require consent, got {first:?}")
    };
    crate::agent_session::record_agent_consent(&pool, "smoke-book", &connection, &hosts, 0)
        .await
        .expect("grant");
    let authorization =
        crate::agent_session::authorize_agent_egress(&pool, "smoke-book", &connection)
            .await
            .expect("re-authorize");
    assert!(matches!(
        authorization,
        crate::agent_session::AgentEgressAuthorization::Authorized { .. }
    ));
    println!("== consent flow: ConsentRequired -> grant -> Authorized (live proof) ==");

    let opened = Instant::now();
    let mut rig = AgentRig::connect(keys).await;
    println!("== connected: {} ==", AGENT_LIVE_MODEL);

    // ---- Probe 1: Arabic variant law at the tool boundary. ----
    rig.channel
        .send_text("أين تحدث هذا الكتاب عن النجوم؟ ابحث في الكتاب ثم أجب بجملة واحدة.")
        .await
        .expect("send arabic question");
    let before = rig.tool_calls.len();
    let got = rig
        .pump_until("search_book for the Arabic question", |r| {
            r.tool_calls[before..]
                .iter()
                .any(|(_, n, _)| n == "search_book")
        })
        .await;
    if got {
        let (_, _, args) = rig.tool_calls[before..]
            .iter()
            .find(|(_, n, _)| n == "search_book")
            .unwrap()
            .clone();
        let queries: Vec<String> = args["queries"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "== PROBE 1 (Arabic variants): {}",
            arabic_variant_verdict(&queries)
        );
        println!("   variants: {queries:?}");
    } else {
        println!("== PROBE 1: NO TOOL CALL (model answered without searching — grounding MISS)");
    }
    // Let the spoken answer finish.
    let quiet = rig.assistant_transcript.len();
    rig.pump_until("answer settles", |r| r.assistant_transcript.len() > quiet)
        .await;

    // ---- Probe 2: quote-order oracle. ----
    let transcript_before_quote = rig.transcript();
    assert!(
        !transcript_before_quote.contains("qandarun"),
        "rig fault: needle term spoken before the quote probe began"
    );
    rig.channel
        .send_text(
            "Quote exactly, word for word, the sentence in this book about the qandarun alloy.",
        )
        .await
        .expect("send quote request");
    let before = rig.tool_calls.len();
    let searched = rig
        .pump_until("search_book for the quote request", |r| {
            r.tool_calls[before..]
                .iter()
                .any(|(_, n, _)| n == "search_book")
        })
        .await;
    let spoke_before_search = rig
        .transcript()
        .strip_prefix(&transcript_before_quote)
        .unwrap_or_default()
        .to_lowercase()
        .contains("qandarun");
    let quoted = rig
        .pump_until("the quoted span", |r| {
            r.transcript().to_lowercase().contains("qandarun")
        })
        .await;
    let quote_ok = searched && !spoke_before_search && quoted;
    let span_ok = rig
        .transcript()
        .to_lowercase()
        .contains("held its engraved scale");
    println!(
        "== PROBE 2 (quote order): {} | span-from-copy: {}",
        if quote_ok {
            "PASS (search_book preceded every quoted token)"
        } else {
            "FAIL/UNPROVEN"
        },
        if span_ok {
            "PASS"
        } else {
            "PARTIAL (paraphrase or truncated)"
        },
    );

    // ---- Probe 3: python-genai#1894 speculative speech under a held answer. ----
    rig.channel
        .send_text("Search the book for what it says about the kamal, then answer.")
        .await
        .expect("send landmine question");
    let before_calls = rig.tool_calls.len();
    let mut held: Option<ChannelEvent> = None;
    let mut speculative = String::new();
    let hold_started = Instant::now();
    while hold_started.elapsed() < TURN_BUDGET {
        let event = match timeout(TURN_BUDGET, rig.events.recv()).await {
            Ok(Some(event)) => event,
            _ => break,
        };
        rig.note(&event);
        match &event {
            ChannelEvent::ToolCall { name, .. } if name == "search_book" && held.is_none() => {
                held = Some(event);
                // Withhold the answer and listen for speculative speech.
                let listen_until = Instant::now() + LANDMINE_HOLD;
                loop {
                    let now = Instant::now();
                    if now >= listen_until {
                        break;
                    }
                    let remaining = listen_until - now;
                    match timeout(remaining, rig.events.recv()).await {
                        Ok(Some(ChannelEvent::TranscriptDelta {
                            role: Role::Assistant,
                            text,
                        })) => {
                            println!("assistant(during hold): {text}");
                            speculative.push_str(&text);
                        }
                        Ok(Some(other)) => rig.note(&other),
                        _ => break,
                    }
                }
                break;
            }
            _ => {
                let owned = event;
                rig.handle(owned).await;
                if rig.tool_calls.len() > before_calls + 3 {
                    break;
                }
            }
        }
    }
    let reproduced = speculative.chars().any(|c| c.is_alphanumeric());
    println!(
        "== PROBE 3 (python-genai#1894): {}",
        if held.is_none() {
            "UNPROVEN (no search_book call arrived to hold)"
        } else if reproduced {
            "REPRODUCED — content spoken before the NON_BLOCKING result returned (recorded, not fixed; owner decision)"
        } else {
            "NOT REPRODUCED in this run (3s hold, one trial)"
        }
    );
    if let Some(call) = held {
        // Answer the held call so the protocol closes cleanly.
        rig.handle(call).await;
        let settled = rig.assistant_transcript.len();
        rig.pump_until("held answer settles", |r| {
            r.assistant_transcript.len() > settled
        })
        .await;
    }

    println!(
        "== SUMMARY: wall-clock {:?}, tool turns {}, assistant transcript {} chars. \
         Usage counters: not surfaced by this channel build — wall-clock and \
         turn counts are the recorded budget facts (Flash-class pennies).",
        opened.elapsed(),
        rig.tool_turns,
        rig.transcript().len(),
    );
    rig.channel.close().await.expect("close");
}
