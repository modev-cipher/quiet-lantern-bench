//! Adapter wiring the voice dispatcher's Book Agent ports (charter B-1) to
//! the B-0 retrieval contract. Fixture harnesses install this on a
//! `Dispatcher` via `set_agent_ports`; the future Agent session wiring (B-2)
//! reuses it unchanged.
//!
//! This is the seam where retrieval internals stop: the dispatcher receives
//! only bounded serialized payloads and [`PassageCoord`]s resolved from
//! opaque session tokens. Sentinel wrapping of the payloads stays at the
//! channel boundary.

use std::sync::Arc;

use serde_json::Value;

use crate::voice::tools::agent::{
    AgentHighlightPort, AgentRetrievalPort, HighlightStatus, PassageCoord,
};

use super::contract::{self, HandleTable};
use super::provider::{Provenance, SearchOptions};
use super::tantivy_provider::TantivyRetrieval;

/// One session's retrieval surface: the provider, the session handle table
/// (whose fingerprint pins the generation), and the owner-tuned options.
pub struct RetrievalPortAdapter {
    provider: Arc<TantivyRetrieval>,
    handles: HandleTable,
    opts: SearchOptions,
}

impl RetrievalPortAdapter {
    /// The full generation fingerprint this port's handles are bound to, for
    /// the session wiring's identity assertion (book, job, content hash,
    /// blocks version — not just the job id).
    pub fn binding_fingerprint(&self) -> &super::provider::BookFingerprint {
        self.handles.fingerprint()
    }

    pub fn new(provider: Arc<TantivyRetrieval>, handles: HandleTable) -> Self {
        Self {
            provider,
            handles,
            opts: SearchOptions::default(),
        }
    }
}

impl AgentRetrievalPort for RetrievalPortAdapter {
    fn job_id(&self) -> &str {
        &self.handles.fingerprint().job_id
    }

    fn search_book(&mut self, queries: &[String], chapter: Option<u32>) -> Value {
        let opts = SearchOptions {
            chapter,
            ..self.opts.clone()
        };
        let result =
            contract::search_book(self.provider.as_ref(), &mut self.handles, queries, &opts);
        let bounded = contract::serialize_bounded(&result);
        serde_json::from_str(&bounded)
            .unwrap_or_else(|_| serde_json::json!({ "status": "invalid" }))
    }

    fn open_passage(&mut self, token: &str) -> Value {
        let result = contract::open_passage(self.provider.as_ref(), &self.handles, token);
        serde_json::to_value(result).unwrap_or_else(|_| serde_json::json!({ "status": "invalid" }))
    }

    fn resolve(&self, token: &str) -> Option<PassageCoord> {
        let (handle, _) = self.handles.resolve(token)?;
        Some(PassageCoord {
            chapter: handle.chapter,
            page: handle.page,
            idx: handle.idx,
        })
    }

    fn resolve_citable(&self, token: &str) -> Option<PassageCoord> {
        let (handle, provenance) = self.handles.resolve(token)?;
        if provenance == Provenance::Generated {
            return None;
        }
        Some(PassageCoord {
            chapter: handle.chapter,
            page: handle.page,
            idx: handle.idx,
        })
    }
}

/// Deterministic highlight surface for fixture harnesses: scripted applied /
/// not-applied answers, recorded calls. The production applier (reading-view
/// round-trip) ships with the visible-UX slice.
pub struct ScriptedHighlightPort {
    /// `None` reason = applied; `Some(reason)` = honest non-application.
    pub refuse_with: Option<String>,
    pub calls: Vec<(PassageCoord, Option<(u32, u32)>)>,
}

impl ScriptedHighlightPort {
    pub fn applying() -> Self {
        Self {
            refuse_with: None,
            calls: Vec::new(),
        }
    }

    pub fn refusing(reason: &str) -> Self {
        Self {
            refuse_with: Some(reason.to_string()),
            calls: Vec::new(),
        }
    }
}

impl AgentHighlightPort for ScriptedHighlightPort {
    fn apply(
        &mut self,
        _call_id: &str,
        coord: PassageCoord,
        span: Option<(u32, u32)>,
    ) -> HighlightStatus {
        self.calls.push((coord, span));
        match &self.refuse_with {
            None => HighlightStatus::Applied,
            Some(reason) => HighlightStatus::NotApplied {
                reason: reason.clone(),
            },
        }
    }
}
