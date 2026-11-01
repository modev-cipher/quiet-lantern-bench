//! Store functions for the voice-session journal (migration 0018;
//! VOICE_AGENT §3/§7).
//!
//! Two tables, two lifetimes: `voice_session` is the renewal-state row the
//! harness writes and renewal/relaunch recovery reads; `voice_transcript` is
//! the retention artifact the 30-day setting governs. The **harness is the
//! sole writer** (§3): channels and tools never touch these rows.
//!
//! This module is the *persisted* side only. Never-persist mode (§7) uses a
//! memory-only twin of the same shapes and never calls into here — the twin
//! and the type both sides live behind are [`crate::voice_journal`], with the
//! runtime, not the store.

use sqlx::SqlitePool;

/// One `voice_session` row — the §3 renewal state for a single session.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceSessionRow {
    pub id: String,
    /// `None` for a standalone session (F6): no book attached, ever — the
    /// nullable columns landed in migration 0024.
    pub book_id: Option<String>,
    /// `None` for a standalone session; also the timeline pin — a position
    /// is only meaningful while this still names the render it was measured
    /// against.
    pub job_id: Option<String>,
    /// Key/consent identity of the provider the session ran on — never key
    /// material (§7).
    pub provider_base_url: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    /// Book-timeline position, ms. Only meaningful while `job_id` still
    /// names the render it was measured against.
    pub playback_pos_ms: i64,
    /// The rolling summary (session-LLM output, §3). `None` until the first
    /// fold runs.
    pub summary_text: Option<String>,
    /// How many transcript turns are folded into `summary_text` so far.
    pub summary_turns: i64,
    /// The typed end reason (F11 ruling #32 b): one of `EndReason::ALL`'s
    /// wire strings; `'ended'` until the session ends.
    pub end_reason: String,
}

/// One `voice_transcript` row — a completed turn, written at turn boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptTurn {
    pub session_id: String,
    pub turn_idx: i64,
    /// `"user"` or `"assistant"` (CHECK-constrained in the schema).
    pub role: String,
    pub text: String,
    pub created_at: i64,
    /// Eviction watermark: true once folded into the rolling summary.
    /// Folding never deletes the row — retention is a separate lifetime.
    pub summarized: bool,
}

pub async fn insert_session(pool: &SqlitePool, row: &VoiceSessionRow) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO voice_session (id, book_id, job_id, provider_base_url, started_at, ended_at,
                                    playback_pos_ms, summary_text, summary_turns, end_reason)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        row.id,
        row.book_id,
        row.job_id,
        row.provider_base_url,
        row.started_at,
        row.ended_at,
        row.playback_pos_ms,
        row.summary_text,
        row.summary_turns,
        row.end_reason,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn get_session(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<VoiceSessionRow>, sqlx::Error> {
    sqlx::query_as!(
        VoiceSessionRow,
        r#"SELECT id as "id!", book_id, job_id, provider_base_url, started_at, ended_at,
                  playback_pos_ms, summary_text, summary_turns, end_reason
           FROM voice_session WHERE id = ?"#,
        id
    )
    .fetch_optional(pool)
    .await
}

/// The most recently started session for a book — the relaunch-recovery read.
/// The caller decides what of it is still usable (a changed/missing `job_id`
/// invalidates the position, never the summary text).
pub async fn latest_session_for_book(
    pool: &SqlitePool,
    book_id: &str,
) -> Result<Option<VoiceSessionRow>, sqlx::Error> {
    sqlx::query_as!(
        VoiceSessionRow,
        r#"SELECT id as "id!", book_id, job_id, provider_base_url, started_at, ended_at,
                  playback_pos_ms, summary_text, summary_turns, end_reason
           FROM voice_session WHERE book_id = ?
           ORDER BY started_at DESC, id DESC LIMIT 1"#,
        book_id
    )
    .fetch_optional(pool)
    .await
}

/// §3 write cadence: every tool-driven playback mutation and pause.
pub async fn update_playback_pos(
    pool: &SqlitePool,
    session_id: &str,
    playback_pos_ms: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE voice_session SET playback_pos_ms = ? WHERE id = ?",
        playback_pos_ms,
        session_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Review 2026-10-09 (T3): the session's coverage, rewritten whole at every
/// completed reply and at session end (runtime-checked query: the column is
/// newer than the offline query cache's other entries need).
pub async fn update_coverage(
    pool: &SqlitePool,
    session_id: &str,
    coverage_json: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE voice_session SET coverage_json = ? WHERE id = ?")
        .bind(coverage_json)
        .bind(session_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The coverage a session recorded, if any (a Continue's source row).
pub async fn get_coverage(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<Option<String>> =
        sqlx::query_scalar("SELECT coverage_json FROM voice_session WHERE id = ?")
            .bind(session_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.flatten())
}

/// One summary fold, atomically: the new summary text lands together with the
/// eviction watermark on the turns it absorbed. A fold that wrote the summary
/// but not the watermark would fold the same turns twice on the next trigger;
/// the reverse would silently drop turns from memory — hence one transaction.
pub async fn record_fold(
    pool: &SqlitePool,
    session_id: &str,
    summary_text: &str,
    through_turn_idx: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let summary_turns = through_turn_idx + 1;
    sqlx::query!(
        "UPDATE voice_session SET summary_text = ?, summary_turns = ? WHERE id = ?",
        summary_text,
        summary_turns,
        session_id,
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE voice_transcript SET summarized = 1 WHERE session_id = ? AND turn_idx <= ?",
        session_id,
        through_turn_idx,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

pub async fn end_session(
    pool: &SqlitePool,
    session_id: &str,
    ended_at: i64,
    end_reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE voice_session SET ended_at = ?, end_reason = ? WHERE id = ?",
        ended_at,
        end_reason,
        session_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// One completed turn (§3: transcript deltas fold at turn boundary — never
/// per-delta writes).
pub async fn append_turn(pool: &SqlitePool, turn: &TranscriptTurn) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO voice_transcript (session_id, turn_idx, role, text, created_at, summarized)
         VALUES (?, ?, ?, ?, ?, ?)",
        turn.session_id,
        turn.turn_idx,
        turn.role,
        turn.text,
        turn.created_at,
        turn.summarized,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// One ANSWERED tool call (4b, owner ruling 2026-09-28: complete
/// tool-execution observability). `ok` is 1 for a successful execution,
/// 0 for every refusal/error; `code` carries the refusal's wire code; `pos_ms`
/// the position a motion reached (or attempted), when the outcome carries one.
pub async fn insert_tool_outcome(
    pool: &SqlitePool,
    session_id: &str,
    tool: &str,
    call_id: &str,
    ok: bool,
    code: Option<&str>,
    pos_ms: Option<u64>,
) -> Result<(), sqlx::Error> {
    let created_at = crate::storage::now_ms();
    let ok_i = ok as i64;
    let pos = pos_ms.map(|value| value as i64);
    sqlx::query!(
        "INSERT INTO voice_tool_outcome
         (session_id, tool, call_id, ok, code, pos_ms, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        session_id,
        tool,
        call_id,
        ok_i,
        code,
        pos,
        created_at,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// In-place update of an existing turn's text (reconciling a provisional turn with authoritative ASR).
pub async fn update_turn_text(
    pool: &SqlitePool,
    session_id: &str,
    turn_idx: i64,
    text: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE voice_transcript SET text = ? WHERE session_id = ? AND turn_idx = ?")
        .bind(text)
        .bind(session_id)
        .bind(turn_idx)
        .execute(pool)
        .await?;
    Ok(())
}

/// Turns not yet folded into the rolling summary, in turn order — the fold
/// input.
pub async fn unsummarized_turns(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<TranscriptTurn>, sqlx::Error> {
    sqlx::query_as!(
        TranscriptTurn,
        r#"SELECT session_id, turn_idx, role, text, created_at, summarized as "summarized: bool"
           FROM voice_transcript WHERE session_id = ? AND summarized = 0
           ORDER BY turn_idx"#,
        session_id
    )
    .fetch_all(pool)
    .await
}

/// The last `k` turns of a session in turn order — the verbatim tail that
/// renewal re-injection keeps (§4 truncation policy).
pub async fn last_turns(
    pool: &SqlitePool,
    session_id: &str,
    k: i64,
) -> Result<Vec<TranscriptTurn>, sqlx::Error> {
    let mut turns = sqlx::query_as!(
        TranscriptTurn,
        r#"SELECT session_id, turn_idx, role, text, created_at, summarized as "summarized: bool"
           FROM voice_transcript WHERE session_id = ?
           ORDER BY turn_idx DESC LIMIT ?"#,
        session_id,
        k
    )
    .fetch_all(pool)
    .await?;
    turns.reverse();
    Ok(turns)
}

/// One row of the history list (F11 §5.2 `list_sessions`): the session row
/// plus the per-row metadata the list needs, read in the same query so the
/// list is one round trip and one consistent snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionListRow {
    pub id: String,
    pub book_id: Option<String>,
    pub job_id: Option<String>,
    pub provider_base_url: String,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub playback_pos_ms: i64,
    pub summary_text: Option<String>,
    pub summary_turns: i64,
    pub end_reason: String,
    /// Sealed turns; never 0 — zero-turn rows (H3) are excluded in SQL.
    pub turn_count: i64,
    /// `created_at` of the newest turn — the H4 derive rule's `ended_at`.
    pub last_turn_at: Option<i64>,
    /// §5.3.1: the job row still exists AND is a cast of this book.
    pub job_resolves: bool,
}

/// The list bound (§5.1). Owner-tunable; an archive deeper than this is
/// truncated from the newest end, never silently reordered.
pub const LIST_LIMIT: i64 = 200;

/// F11 §5.2: the history list for one scope — `Some(book)` for a book's rows,
/// `None` for standalone rows — newest first, bounded by [`LIST_LIMIT`].
/// `exclude_id` names the live row, which is not history yet and must not
/// eat one of the bounded slots (excluded in SQL, not after the LIMIT).
/// Scope uses SQLite's null-safe `IS` so one bind serves both scopes and
/// the partial index on `book_id` stays usable.
/// Rows with zero sealed turns never come out (A3: the inner join on
/// `voice_transcript` is the exclusion). Read-only: the H4 derive happens in
/// the caller from `ended_at`/`last_turn_at`; no sweep writes here (D-9 (a)).
pub async fn list_sessions(
    pool: &SqlitePool,
    scope: Option<&str>,
    exclude_id: Option<&str>,
) -> Result<Vec<SessionListRow>, sqlx::Error> {
    sqlx::query_as!(
        SessionListRow,
        r#"SELECT s.id AS "id!", s.book_id, s.job_id, s.provider_base_url AS "provider_base_url!", s.started_at AS "started_at!",
                  s.ended_at, s.playback_pos_ms AS "playback_pos_ms!", s.summary_text,
                  s.summary_turns AS "summary_turns!", s.end_reason AS "end_reason!",
                  COUNT(t.turn_idx) AS "turn_count!: i64",
                  MAX(t.created_at) AS "last_turn_at: i64",
                  (j.id IS NOT NULL) AS "job_resolves!: bool"
           FROM voice_session s
           JOIN voice_transcript t ON t.session_id = s.id
           LEFT JOIN render_job j ON j.id = s.job_id AND j.book_id = s.book_id
           WHERE s.book_id IS ? AND s.id IS NOT ?
           GROUP BY s.id
           ORDER BY s.started_at DESC, s.id DESC
           LIMIT ?"#,
        scope,
        exclude_id,
        LIST_LIMIT
    )
    .fetch_all(pool)
    .await
}

/// One history row by id, same shape and same H3 rule as [`list_sessions`]
/// (`None` for a missing row and for a zero-turn row alike — neither is
/// history).
pub async fn list_row(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Option<SessionListRow>, sqlx::Error> {
    sqlx::query_as!(
        SessionListRow,
        r#"SELECT s.id AS "id!", s.book_id, s.job_id, s.provider_base_url AS "provider_base_url!", s.started_at AS "started_at!",
                  s.ended_at, s.playback_pos_ms AS "playback_pos_ms!", s.summary_text,
                  s.summary_turns AS "summary_turns!", s.end_reason AS "end_reason!",
                  COUNT(t.turn_idx) AS "turn_count!: i64",
                  MAX(t.created_at) AS "last_turn_at: i64",
                  (j.id IS NOT NULL) AS "job_resolves!: bool"
           FROM voice_session s
           JOIN voice_transcript t ON t.session_id = s.id
           LEFT JOIN render_job j ON j.id = s.job_id AND j.book_id = s.book_id
           WHERE s.id = ?
           GROUP BY s.id"#,
        session_id
    )
    .fetch_optional(pool)
    .await
}

pub async fn set_turn_voiceprint(
    pool: &SqlitePool,
    session_id: &str,
    turn_idx: i64,
    bars: &[u8],
) -> Result<(), sqlx::Error> {
    let encoded: String = bars.iter().map(|bar| format!("{bar:02x}")).collect();
    sqlx::query("UPDATE voice_transcript SET voiceprint = ? WHERE session_id = ? AND turn_idx = ?")
        .bind(encoded)
        .bind(session_id)
        .bind(turn_idx)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn turn_voiceprints(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<std::collections::HashMap<i64, Vec<u8>>, sqlx::Error> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT turn_idx, voiceprint FROM voice_transcript \
         WHERE session_id = ? AND voiceprint IS NOT NULL",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(turn_idx, encoded)| decode_voiceprint(&encoded).map(|bars| (turn_idx, bars)))
        .collect())
}

fn decode_voiceprint(encoded: &str) -> Option<Vec<u8>> {
    if encoded.len() != crate::voice::mic::VOICEPRINT_BARS * 2 || !encoded.is_ascii() {
        return None;
    }
    (0..encoded.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&encoded[at..at + 2], 16).ok())
        .collect()
}

/// Every sealed turn of a session in `turn_idx` order — the transcript view.
pub async fn all_turns(
    pool: &SqlitePool,
    session_id: &str,
) -> Result<Vec<TranscriptTurn>, sqlx::Error> {
    sqlx::query_as!(
        TranscriptTurn,
        r#"SELECT session_id, turn_idx, role, text, created_at, summarized as "summarized: bool"
           FROM voice_transcript WHERE session_id = ?
           ORDER BY turn_idx"#,
        session_id
    )
    .fetch_all(pool)
    .await
}

pub async fn turn_count(pool: &SqlitePool, session_id: &str) -> Result<i64, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) AS "n!: i64" FROM voice_transcript WHERE session_id = ?"#,
        session_id
    )
    .fetch_one(pool)
    .await?;
    Ok(row.n)
}

/// §6.3 row delete: the row goes and the FK cascade takes its transcript
/// rows; no soft-delete. Returns whether a row existed. The live guard is the
/// caller's (the store cannot see the slot).
pub async fn delete_session(pool: &SqlitePool, session_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM voice_session WHERE id = ?", session_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// §6.3 scope forget: every row in the scope (`Some(book)` / `None` =
/// standalone), zero-turn rows included (DQA-H8), except the row named
/// `keep` — the live row, which survives. Returns the number deleted.
pub async fn delete_scope(
    pool: &SqlitePool,
    scope: Option<&str>,
    keep: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query!(
        "DELETE FROM voice_session WHERE book_id IS ? AND id IS NOT ?",
        scope,
        keep
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// [R2] The cast-delete preview count: every `voice_session` row the
/// `job_id` cascade will take, zero-turn rows included (D12 — this counts
/// what cascades, not what is listed).
pub async fn count_for_job(pool: &SqlitePool, job_id: &str) -> Result<u64, sqlx::Error> {
    let row = sqlx::query!(
        r#"SELECT COUNT(*) AS "n!: i64" FROM voice_session WHERE job_id = ?"#,
        job_id
    )
    .fetch_one(pool)
    .await?;
    Ok(row.n.max(0) as u64)
}
/// A mixed history fixture shared by the storage pins and the command pins
/// (F11 §7): one book with two casts, every §2.2 row shape that can coexist.
#[cfg(test)]
pub(crate) mod fixture {
    use super::*;

    pub(crate) const BOOK: &str = "b1";
    pub(crate) const JOB_A: &str = "job-a";
    pub(crate) const JOB_B: &str = "job-b";
    pub(crate) const PROVIDER: &str = "https://p.example";
    /// H7 on cast A: two turns, a summary, ended.
    pub(crate) const S_A: &str = "s-a";
    /// H7 on cast B: one turn, ended.
    pub(crate) const S_B: &str = "s-b";
    /// H6 BookDirect: no job, one turn, ended.
    pub(crate) const S_DIRECT: &str = "s-direct";
    /// H3 on cast A: ended with zero turns.
    pub(crate) const S_EMPTY: &str = "s-empty";
    /// H4 orphan-live on cast A: one turn, `ended_at` NULL.
    pub(crate) const S_ORPHAN: &str = "s-orphan";
    /// H5 standalone: one turn, ended.
    pub(crate) const S_SOLO: &str = "s-solo";
    /// H3 standalone: ended with zero turns.
    pub(crate) const S_SOLO_EMPTY: &str = "s-solo-empty";

    /// `(id, book_id, job_id, started_at, ended_at, summary_text, turns)`.
    type FixtureRow<'a> = (
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
        i64,
        Option<i64>,
        Option<&'a str>,
        usize,
    );

    pub(crate) async fn mixed_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::storage::open_pool(&dir.path().join("history.db"))
            .await
            .unwrap();
        for sql in [
            "INSERT INTO book (id, title, source_path, format, content_hash, created_at)
             VALUES ('b1', 'B', '/tmp/b.pdf', 'pdf', 'h1', 1)",
            "INSERT INTO voice_profile (id, name, engine, kind, created_at)
             VALUES ('vp1', 'p', 'kokoro', 'preset', 1)",
            "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id,
                                     output_format, state, created_at, updated_at)
             VALUES ('job-a', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'DONE', 1, 1)",
            "INSERT INTO render_job (id, book_id, scope_json, mode, voice_profile_id,
                                     output_format, state, created_at, updated_at)
             VALUES ('job-b', 'b1', '{}', 'teacher', 'vp1', 'mp3', 'DONE', 2, 2)",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        let rows: [FixtureRow; 7] = [
            (
                S_A,
                Some(BOOK),
                Some(JOB_A),
                1000,
                Some(1500),
                Some("Inertia was defined. Then momentum."),
                2,
            ),
            (S_B, Some(BOOK), Some(JOB_B), 2000, Some(2500), None, 1),
            (S_DIRECT, Some(BOOK), None, 3000, Some(3500), None, 1),
            (S_EMPTY, Some(BOOK), Some(JOB_A), 4000, Some(4500), None, 0),
            (S_ORPHAN, Some(BOOK), Some(JOB_A), 5000, None, None, 1),
            (S_SOLO, None, None, 6000, Some(6500), None, 1),
            (S_SOLO_EMPTY, None, None, 7000, Some(7500), None, 0),
        ];
        for (id, book_id, job_id, started_at, ended_at, summary, turns) in rows {
            insert_session(
                &pool,
                &VoiceSessionRow {
                    id: id.to_string(),
                    book_id: book_id.map(str::to_string),
                    job_id: job_id.map(str::to_string),
                    provider_base_url: PROVIDER.to_string(),
                    started_at,
                    ended_at,
                    playback_pos_ms: started_at * 10,
                    summary_text: summary.map(str::to_string),
                    summary_turns: 0,
                    end_reason: "ended".to_string(),
                },
            )
            .await
            .unwrap();
            for idx in 0..turns as i64 {
                append_turn(
                    &pool,
                    &TranscriptTurn {
                        session_id: id.to_string(),
                        turn_idx: idx,
                        role: if idx % 2 == 0 { "user" } else { "assistant" }.to_string(),
                        text: format!("{id} turn {idx}"),
                        created_at: started_at + 100 * (idx + 1),
                        summarized: false,
                    },
                )
                .await
                .unwrap();
            }
        }
        (dir, pool)
    }

    pub(crate) async fn session_ids(pool: &SqlitePool) -> Vec<String> {
        use sqlx::Row;
        sqlx::query("SELECT id FROM voice_session ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.get::<String, _>("id"))
            .collect()
    }

    pub(crate) async fn transcript_count(pool: &SqlitePool, session_id: &str) -> i64 {
        use sqlx::Row;
        sqlx::query("SELECT COUNT(*) AS n FROM voice_transcript WHERE session_id = ?")
            .bind(session_id)
            .fetch_one(pool)
            .await
            .unwrap()
            .get::<i64, _>("n")
    }

    /// Row + every turn: the byte-identity snapshot D11 compares.
    pub(crate) async fn snapshot(
        pool: &SqlitePool,
        session_id: &str,
    ) -> (Option<VoiceSessionRow>, Vec<TranscriptTurn>) {
        (
            get_session(pool, session_id).await.unwrap(),
            all_turns(pool, session_id).await.unwrap(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    fn ids(rows: &[SessionListRow]) -> Vec<&str> {
        rows.iter().map(|r| r.id.as_str()).collect()
    }

    /// A3: zero-turn rows (H3) never come out of the list query, in either scope.
    #[tokio::test]
    async fn a3_list_sessions_excludes_rows_with_zero_turns() {
        let (_dir, pool) = mixed_pool().await;
        let book = list_sessions(&pool, Some(BOOK), None).await.unwrap();
        assert!(
            !ids(&book).contains(&S_EMPTY),
            "H3 book row listed: {:?}",
            ids(&book)
        );
        let solo = list_sessions(&pool, None, None).await.unwrap();
        assert!(
            !ids(&solo).contains(&S_SOLO_EMPTY),
            "H3 standalone row listed"
        );
        assert_eq!(book.iter().find(|r| r.id == S_A).unwrap().turn_count, 2);
        assert!(book.iter().all(|r| r.turn_count >= 1));
    }

    /// A5: book rows and standalone rows are disjoint sets; newest first.
    #[tokio::test]
    async fn a5_book_and_standalone_scopes_are_disjoint_and_newest_first() {
        let (_dir, pool) = mixed_pool().await;
        let book = list_sessions(&pool, Some(BOOK), None).await.unwrap();
        let solo = list_sessions(&pool, None, None).await.unwrap();
        assert_eq!(ids(&book), vec![S_ORPHAN, S_DIRECT, S_B, S_A]);
        assert_eq!(ids(&solo), vec![S_SOLO]);
        assert!(ids(&book).iter().all(|id| !ids(&solo).contains(id)));
    }

    /// §5.1: bounded at 200 rows; per-row metadata (`last_turn_at`,
    /// `job_resolves`) is read in the same query.
    #[tokio::test]
    async fn list_sessions_is_bounded_and_carries_last_turn_and_job_resolution() {
        let (_dir, pool) = mixed_pool().await;
        for n in 0..205i64 {
            let id = format!("bulk-{n:03}");
            insert_session(
                &pool,
                &VoiceSessionRow {
                    id: id.clone(),
                    book_id: None,
                    job_id: None,
                    provider_base_url: PROVIDER.to_string(),
                    started_at: 10_000 + n,
                    ended_at: Some(10_001 + n),
                    playback_pos_ms: 0,
                    summary_text: None,
                    summary_turns: 0,
                    end_reason: "ended".to_string(),
                },
            )
            .await
            .unwrap();
            append_turn(
                &pool,
                &TranscriptTurn {
                    session_id: id,
                    turn_idx: 0,
                    role: "user".into(),
                    text: "x".into(),
                    created_at: 10_000 + n,
                    summarized: false,
                },
            )
            .await
            .unwrap();
        }
        let solo = list_sessions(&pool, None, None).await.unwrap();
        assert_eq!(solo.len(), 200);
        assert_eq!(solo[0].id, "bulk-204");
        let book = list_sessions(&pool, Some(BOOK), None).await.unwrap();
        let a = book.iter().find(|r| r.id == S_A).unwrap();
        assert_eq!(a.last_turn_at, Some(1200));
        assert!(
            a.job_resolves,
            "job A still exists and is a cast of this book"
        );
        let direct = book.iter().find(|r| r.id == S_DIRECT).unwrap();
        assert!(
            !direct.job_resolves,
            "a BookDirect row has no job to resolve"
        );
        assert_eq!(direct.last_turn_at, Some(3100));
    }

    #[tokio::test]
    async fn all_turns_returns_every_turn_in_turn_idx_order_and_turn_count_agrees() {
        let (_dir, pool) = mixed_pool().await;
        let turns = all_turns(&pool, S_A).await.unwrap();
        assert_eq!(
            turns.iter().map(|t| t.turn_idx).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert_eq!(turns[1].text, "s-a turn 1");
        assert_eq!(turn_count(&pool, S_A).await.unwrap(), 2);
        assert_eq!(turn_count(&pool, S_EMPTY).await.unwrap(), 0);
        assert!(all_turns(&pool, S_EMPTY).await.unwrap().is_empty());
    }

    /// D4 (storage half): the row and its transcript rows go together; nothing
    /// else moves.
    #[tokio::test]
    async fn d4_delete_session_removes_the_row_and_its_transcript_rows_only() {
        let (_dir, pool) = mixed_pool().await;
        let before = session_ids(&pool).await;
        assert_eq!(transcript_count(&pool, S_A).await, 2);
        assert!(delete_session(&pool, S_A).await.unwrap());
        assert!(get_session(&pool, S_A).await.unwrap().is_none());
        assert_eq!(transcript_count(&pool, S_A).await, 0);
        let after = session_ids(&pool).await;
        assert_eq!(after.len(), before.len() - 1);
        assert!(before
            .iter()
            .filter(|id| *id != S_A)
            .all(|id| after.contains(id)));
        assert_eq!(transcript_count(&pool, S_B).await, 1);
        assert!(
            !delete_session(&pool, S_A).await.unwrap(),
            "second delete finds nothing"
        );
    }

    /// D5: every row in scope goes, none outside it, and the row named
    /// `keep` (the live row) survives (§6.3 "The live session is kept").
    #[tokio::test]
    async fn d5_delete_scope_removes_the_scope_keeps_the_live_row_and_nothing_outside() {
        let (_dir, pool) = mixed_pool().await;
        let deleted = delete_scope(&pool, Some(BOOK), Some(S_ORPHAN))
            .await
            .unwrap();
        assert_eq!(deleted, 4, "a, b, direct and the hidden empty row");
        assert_eq!(
            session_ids(&pool).await,
            vec![S_ORPHAN, S_SOLO, S_SOLO_EMPTY]
        );
        assert_eq!(transcript_count(&pool, S_ORPHAN).await, 1);
        assert_eq!(transcript_count(&pool, S_SOLO).await, 1);
        assert_eq!(transcript_count(&pool, S_A).await, 0);
    }

    /// DQA-H8: the zero-turn row is invisible to the list but forget-scope
    /// deletes it and counts it.
    #[tokio::test]
    async fn dqa_h8_forget_scope_deletes_and_counts_the_hidden_zero_turn_row() {
        let (_dir, pool) = mixed_pool().await;
        let listed = list_sessions(&pool, None, None).await.unwrap();
        assert!(!ids(&listed).contains(&S_SOLO_EMPTY), "never shown");
        let deleted = delete_scope(&pool, None, None).await.unwrap();
        assert_eq!(deleted, 2, "the listed standalone row and the hidden one");
        assert!(get_session(&pool, S_SOLO_EMPTY).await.unwrap().is_none());
        assert!(get_session(&pool, S_SOLO).await.unwrap().is_none());
        assert!(
            get_session(&pool, S_A).await.unwrap().is_some(),
            "book rows untouched"
        );
    }

    /// D12: the preview count is what the cascade will take, zero-turn rows
    /// included — not what the list shows.
    #[tokio::test]
    async fn d12_count_for_job_counts_zero_turn_rows_too() {
        let (_dir, pool) = mixed_pool().await;
        assert_eq!(
            count_for_job(&pool, JOB_A).await.unwrap(),
            3,
            "a, empty, orphan"
        );
        assert_eq!(count_for_job(&pool, JOB_B).await.unwrap(), 1);
        assert_eq!(count_for_job(&pool, "no-such-job").await.unwrap(), 0);
        let listed = list_sessions(&pool, Some(BOOK), None).await.unwrap();
        let listed_on_a = listed
            .iter()
            .filter(|r| r.job_id.as_deref() == Some(JOB_A))
            .count();
        assert_eq!(
            listed_on_a, 2,
            "the list hides the H3 row the count includes"
        );
    }

    /// D11 [R2]: `delete_job` cascades exactly that cast's rows and
    /// transcripts; BookDirect and Standalone rows survive byte-identical.
    #[tokio::test]
    async fn d11_delete_job_cascades_only_that_casts_rows_and_transcripts() {
        let (_dir, pool) = mixed_pool().await;
        let direct_before = snapshot(&pool, S_DIRECT).await;
        let solo_before = snapshot(&pool, S_SOLO).await;
        let b_before = snapshot(&pool, S_B).await;
        assert!(crate::storage::job::delete_job_if_settled(&pool, JOB_A)
            .await
            .unwrap());
        for gone in [S_A, S_EMPTY, S_ORPHAN] {
            assert!(
                get_session(&pool, gone).await.unwrap().is_none(),
                "{gone} survived"
            );
            assert_eq!(
                transcript_count(&pool, gone).await,
                0,
                "{gone} transcript survived"
            );
        }
        assert_eq!(snapshot(&pool, S_DIRECT).await, direct_before);
        assert_eq!(snapshot(&pool, S_SOLO).await, solo_before);
        assert_eq!(snapshot(&pool, S_B).await, b_before);
        assert!(direct_before.0.is_some() && solo_before.0.is_some());
    }

    /// DQA-H9 [R2]: after cast A goes, the book's list shows B and the
    /// BookDirect row; the standalone list is unchanged.
    #[tokio::test]
    async fn dqa_h9_cast_cascade_leaves_the_other_cast_bookdirect_and_standalone_listed() {
        let (_dir, pool) = mixed_pool().await;
        assert!(crate::storage::job::delete_job_if_settled(&pool, JOB_A)
            .await
            .unwrap());
        let book = list_sessions(&pool, Some(BOOK), None).await.unwrap();
        assert_eq!(ids(&book), vec![S_DIRECT, S_B]);
        assert_eq!(
            ids(&list_sessions(&pool, None, None).await.unwrap()),
            vec![S_SOLO]
        );
        assert_eq!(transcript_count(&pool, S_B).await, 1);
    }

    #[tokio::test]
    async fn update_turn_text_modifies_existing_turn_in_place() {
        let (_dir, pool) = mixed_pool().await;
        let before = last_turns(&pool, S_A, 2).await.unwrap();
        assert_eq!(before[0].turn_idx, 0);
        assert_eq!(before[0].text, format!("{S_A} turn 0"));

        update_turn_text(&pool, S_A, 0, "updated text reconciled from stream")
            .await
            .unwrap();

        let after = last_turns(&pool, S_A, 2).await.unwrap();
        assert_eq!(after[0].turn_idx, 0);
        assert_eq!(after[0].text, "updated text reconciled from stream");
        assert_eq!(
            after.len(),
            before.len(),
            "turn count preserved, zero duplicate rows"
        );
    }
}
