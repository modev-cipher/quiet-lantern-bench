//! The cloud Script path's port of the engine's script contract (charter
//! §3.4). The split seam, decided for S-1: everything the provider touches —
//! chunk budgeting, prompts, output budgets, the three-attempt repair
//! ladder, marker validation, rolling summaries — runs here in Rust around
//! the `ScriptLlm` trait, so keys never leave Rust custody; unit assembly
//! and pysbd sentence segmentation stay in the engine behind local
//! endpoints (pure-local text processing, exactly like the align handoff
//! charter §3.2 already sanctions), because a Rust re-implementation of
//! pysbd could not honestly prove segment parity.
//!
//! Every constant and prompt string below is a verbatim port of
//! `engine/txtcaster_engine/script/teacher.py` (and `chunker.py` /
//! `cues.rs` / `shape.py` for their parts); the shared fixture corpus in
//! `teacher.rs` tests pins the two implementations together.

pub mod chunker;
pub mod cues;
pub mod shape;
pub mod teacher;
