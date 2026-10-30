//! The `usage_turns` and `usage_limits` tables (schema v19, limits per account from v21,
//! `docs/agents.md` § Usage).
//!
//! Writes are applied before they return (they go through the writer like a read): a limit the
//! daemon announced is on disk when the announcement is.

use hutan_core::SessionId;
use rusqlite::{Connection, params};

use crate::Storage;
use crate::error::{Result, StorageError};

/// How long a turn's row is kept: the summary only reads the last week, a month leaves room
/// for a later view without growing without bound.
pub const USAGE_KEPT_DAYS: u64 = 35;

/// What one turn used.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageTurnRow {
    pub session_id: SessionId,
    pub turn_id: String,
    /// When the turn ended.
    pub at_ms: u64,
    /// `codex`, `omp`, `claude` or `direct`.
    pub harness: String,
    /// The model provider (`anthropic`, `openai`, a direct chat's provider id).
    pub provider: String,
    pub model: Option<String>,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    /// The reported total, else input plus output.
    pub total_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// The turns of one harness and provider that ended in a period, summed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UsageSum {
    pub harness: String,
    pub provider: String,
    pub turns: u64,
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
    /// `None` while no turn reported a cost.
    pub cost_usd: Option<f64>,
}

/// The newest report of one limit window.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageLimitRow {
    pub harness: String,
    /// The managed account the window belongs to; empty: the harness's own login.
    pub account: String,
    pub provider: String,
    pub window_id: String,
    pub label: String,
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    pub resets_at_ms: Option<u64>,
    pub plan: Option<String>,
    pub observed_at_ms: u64,
}

/// Usage records.
#[derive(Debug, Clone, Copy)]
pub struct Usage<'a>(pub(crate) &'a Storage);

impl Usage<'_> {
    /// Records a turn (a turn already recorded is replaced) and forgets the turns older than
    /// [`USAGE_KEPT_DAYS`].
    pub fn record_turn(&self, row: UsageTurnRow) -> Result<()> {
        self.0.read(move |conn| {
            let context = "recording a turn's usage";
            conn.execute(
                "INSERT OR REPLACE INTO usage_turns (session_id, turn_id, at_ms, harness, \
                     provider, model, input_tokens, cached_input_tokens, output_tokens, \
                     reasoning_output_tokens, total_tokens, cost_usd) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    row.session_id.to_string(),
                    row.turn_id,
                    ms(row.at_ms),
                    row.harness,
                    row.provider,
                    row.model,
                    row.input_tokens.map(ms),
                    row.cached_input_tokens.map(ms),
                    row.output_tokens.map(ms),
                    row.reasoning_output_tokens.map(ms),
                    row.total_tokens.map(ms),
                    row.cost_usd,
                ],
            )
            .map_err(StorageError::sqlite(context))?;
            let cutoff = row.at_ms.saturating_sub(USAGE_KEPT_DAYS * 86_400_000);
            conn.execute("DELETE FROM usage_turns WHERE at_ms < ?1", [ms(cutoff)])
                .map_err(StorageError::sqlite(context))?;
            Ok(())
        })
    }

    /// The turns that ended in `since_ms..=until_ms`, oldest first (as far as the records go
    /// back: [`USAGE_KEPT_DAYS`] days).
    pub fn turns_in_window(&self, since_ms: u64, until_ms: u64) -> Result<Vec<UsageTurnRow>> {
        self.0.read(move |conn| {
            let context = "reading the turns of a window";
            let mut stmt = conn
                .prepare(
                    "SELECT session_id, turn_id, at_ms, harness, provider, model, input_tokens, \
                         cached_input_tokens, output_tokens, reasoning_output_tokens, \
                         total_tokens, cost_usd \
                     FROM usage_turns WHERE at_ms BETWEEN ?1 AND ?2 ORDER BY at_ms, turn_id",
                )
                .map_err(StorageError::sqlite(context))?;
            let rows = stmt
                .query_map(params![ms(since_ms), ms(until_ms)], |row| {
                    let tokens = |at: usize| -> rusqlite::Result<Option<u64>> {
                        Ok(row.get::<_, Option<i64>>(at)?.map(unms))
                    };
                    Ok((
                        row.get::<_, String>(0)?,
                        UsageTurnRow {
                            session_id: SessionId::new(),
                            turn_id: row.get(1)?,
                            at_ms: unms(row.get(2)?),
                            harness: row.get(3)?,
                            provider: row.get(4)?,
                            model: row.get(5)?,
                            input_tokens: tokens(6)?,
                            cached_input_tokens: tokens(7)?,
                            output_tokens: tokens(8)?,
                            reasoning_output_tokens: tokens(9)?,
                            total_tokens: tokens(10)?,
                            cost_usd: row.get(11)?,
                        },
                    ))
                })
                .map_err(StorageError::sqlite(context))?;
            let mut out = Vec::new();
            for row in rows {
                let (session, mut turn) = row.map_err(StorageError::sqlite(context))?;
                turn.session_id = session
                    .parse()
                    .map_err(|e| StorageError::corrupt("usage_turns.session_id", e))?;
                out.push(turn);
            }
            Ok(out)
        })
    }

    /// The newest `limit` turns of one session (as far as the records go back: they are kept
    /// [`USAGE_KEPT_DAYS`] days), oldest first.
    pub fn turns_of(&self, session: SessionId, limit: u32) -> Result<Vec<UsageTurnRow>> {
        self.0.read(move |conn| {
            let context = "reading a session's turns";
            let mut stmt = conn
                .prepare(
                    "SELECT turn_id, at_ms, harness, provider, model, input_tokens, \
                         cached_input_tokens, output_tokens, reasoning_output_tokens, \
                         total_tokens, cost_usd \
                     FROM usage_turns WHERE session_id = ?1 ORDER BY at_ms DESC LIMIT ?2",
                )
                .map_err(StorageError::sqlite(context))?;
            let rows = stmt
                .query_map(params![session.to_string(), i64::from(limit)], |row| {
                    let tokens = |at: usize| -> rusqlite::Result<Option<u64>> {
                        Ok(row.get::<_, Option<i64>>(at)?.map(unms))
                    };
                    Ok(UsageTurnRow {
                        session_id: session,
                        turn_id: row.get(0)?,
                        at_ms: unms(row.get(1)?),
                        harness: row.get(2)?,
                        provider: row.get(3)?,
                        model: row.get(4)?,
                        input_tokens: tokens(5)?,
                        cached_input_tokens: tokens(6)?,
                        output_tokens: tokens(7)?,
                        reasoning_output_tokens: tokens(8)?,
                        total_tokens: tokens(9)?,
                        cost_usd: row.get(10)?,
                    })
                })
                .map_err(StorageError::sqlite(context))?;
            let mut turns = rows
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(StorageError::sqlite(context))?;
            turns.reverse();
            Ok(turns)
        })
    }

    /// The turns that ended at or after `since_ms`, summed per harness and provider.
    pub fn sums_since(&self, since_ms: u64) -> Result<Vec<UsageSum>> {
        self.0.read(move |conn| sums_since(conn, since_ms))
    }

    /// Stores windows, each replacing the one with the same harness, account, provider and
    /// window id.
    pub fn upsert_limits(&self, rows: Vec<UsageLimitRow>) -> Result<()> {
        self.0.read(move |conn| {
            let tx = conn
                .unchecked_transaction()
                .map_err(StorageError::sqlite("recording limits"))?;
            for row in &rows {
                upsert_limit(&tx, row)?;
            }
            tx.commit()
                .map_err(StorageError::sqlite("recording limits"))
        })
    }

    /// Replaces every window of `harness` and `account` (empty: its own login) with `rows` (a
    /// harness that reports all of its windows at once: a window it no longer reports is gone).
    pub fn replace_limits(
        &self,
        harness: String,
        account: String,
        rows: Vec<UsageLimitRow>,
    ) -> Result<()> {
        self.0.read(move |conn| {
            let context = "replacing limits";
            let tx = conn
                .unchecked_transaction()
                .map_err(StorageError::sqlite(context))?;
            tx.execute(
                "DELETE FROM usage_limits WHERE harness = ?1 AND account = ?2",
                [&harness, &account],
            )
            .map_err(StorageError::sqlite(context))?;
            for row in &rows {
                upsert_limit(&tx, row)?;
            }
            tx.commit().map_err(StorageError::sqlite(context))
        })
    }

    pub fn limits(&self) -> Result<Vec<UsageLimitRow>> {
        self.0.read(|conn| {
            let context = "reading limits";
            let mut stmt = conn
                .prepare(
                    "SELECT harness, account, provider, window_id, label, used_percent, window_minutes, \
                         resets_at_ms, plan, observed_at_ms \
                     FROM usage_limits \
                     ORDER BY harness, account, provider, window_minutes, window_id",
                )
                .map_err(StorageError::sqlite(context))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(UsageLimitRow {
                        harness: row.get(0)?,
                        account: row.get(1)?,
                        provider: row.get(2)?,
                        window_id: row.get(3)?,
                        label: row.get(4)?,
                        used_percent: row.get(5)?,
                        window_minutes: row.get::<_, Option<i64>>(6)?.map(unms),
                        resets_at_ms: row.get::<_, Option<i64>>(7)?.map(unms),
                        plan: row.get(8)?,
                        observed_at_ms: unms(row.get(9)?),
                    })
                })
                .map_err(StorageError::sqlite(context))?;
            rows.map(|row| row.map_err(StorageError::sqlite(context)))
                .collect()
        })
    }
}

fn upsert_limit(conn: &Connection, row: &UsageLimitRow) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO usage_limits (harness, account, provider, window_id, label, \
             used_percent, window_minutes, resets_at_ms, plan, observed_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            row.harness,
            row.account,
            row.provider,
            row.window_id,
            row.label,
            row.used_percent,
            row.window_minutes.map(ms),
            row.resets_at_ms.map(ms),
            row.plan,
            ms(row.observed_at_ms),
        ],
    )
    .map_err(StorageError::sqlite("recording a limit"))?;
    Ok(())
}

fn sums_since(conn: &Connection, since_ms: u64) -> Result<Vec<UsageSum>> {
    let context = "summing usage";
    let mut stmt = conn
        .prepare(
            "SELECT harness, provider, COUNT(*), \
                 COALESCE(SUM(input_tokens), 0), COALESCE(SUM(cached_input_tokens), 0), \
                 COALESCE(SUM(output_tokens), 0), COALESCE(SUM(reasoning_output_tokens), 0), \
                 COALESCE(SUM(total_tokens), 0), SUM(cost_usd) \
             FROM usage_turns WHERE at_ms >= ?1 \
             GROUP BY harness, provider ORDER BY harness, provider",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([ms(since_ms)], |row| {
            Ok(UsageSum {
                harness: row.get(0)?,
                provider: row.get(1)?,
                turns: unms(row.get(2)?),
                input_tokens: unms(row.get(3)?),
                cached_input_tokens: unms(row.get(4)?),
                output_tokens: unms(row.get(5)?),
                reasoning_output_tokens: unms(row.get(6)?),
                total_tokens: unms(row.get(7)?),
                cost_usd: row.get(8)?,
            })
        })
        .map_err(StorageError::sqlite(context))?;
    rows.map(|row| row.map_err(StorageError::sqlite(context)))
        .collect()
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage() -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path().join("hutan.db")).unwrap();
        (dir, storage)
    }

    fn turn(turn: &str, at_ms: u64, harness: &str, tokens: u64, cost: Option<f64>) -> UsageTurnRow {
        UsageTurnRow {
            session_id: SessionId::new(),
            turn_id: turn.to_owned(),
            at_ms,
            harness: harness.to_owned(),
            provider: "anthropic".to_owned(),
            model: Some("claude-haiku-5-5".to_owned()),
            input_tokens: Some(tokens),
            cached_input_tokens: None,
            output_tokens: Some(1),
            reasoning_output_tokens: None,
            total_tokens: Some(tokens + 1),
            cost_usd: cost,
        }
    }

    #[test]
    fn the_turns_of_a_window_come_with_their_session_oldest_first() {
        let (_dir, storage) = storage();
        let usage = storage.usage();
        let (a, b) = (
            turn("a", 100, "omp", 5, None),
            turn("b", 200, "claude", 7, Some(0.1)),
        );
        let c = turn("c", 900, "omp", 9, None);
        for row in [&c, &b, &a] {
            usage.record_turn(row.clone()).unwrap();
        }
        let inside = usage.turns_in_window(150, 500).unwrap();
        assert_eq!(inside, std::slice::from_ref(&b));
        let both = usage.turns_in_window(0, 500).unwrap();
        assert_eq!(both, [a, b]);
    }

    #[test]
    fn turns_sum_per_harness_from_a_start_and_old_ones_are_forgotten() {
        let (_dir, storage) = storage();
        let usage = storage.usage();
        let day = 86_400_000;
        let now = 100 * day;
        usage
            .record_turn(turn("a", now - 40 * day, "omp", 5, None))
            .unwrap();
        usage
            .record_turn(turn("b", now - 2 * day, "omp", 10, Some(0.5)))
            .unwrap();
        usage
            .record_turn(turn("c", now - 1000, "omp", 20, Some(0.25)))
            .unwrap();
        usage
            .record_turn(turn("d", now, "codex", 30, None))
            .unwrap();
        // The turn of 40 days ago went with the write of today's.
        let week = usage.sums_since(now - 7 * day).unwrap();
        assert_eq!(week.len(), 2);
        let omp = &week[1];
        assert_eq!(
            (omp.harness.as_str(), omp.turns, omp.total_tokens),
            ("omp", 2, 32)
        );
        assert_eq!(omp.input_tokens, 30);
        assert_eq!(omp.cost_usd, Some(0.75));
        let codex = &week[0];
        assert_eq!(codex.cost_usd, None, "no turn reported a cost");
        let today = usage.sums_since(now - 500).unwrap();
        assert_eq!(today.len(), 1);
        assert_eq!(today[0].harness, "codex");
        assert_eq!(
            usage
                .sums_since(0)
                .unwrap()
                .iter()
                .map(|s| s.turns)
                .sum::<u64>(),
            3
        );
    }

    #[test]
    fn a_turn_recorded_twice_counts_once() {
        let (_dir, storage) = storage();
        let usage = storage.usage();
        let row = turn("a", 1000, "omp", 10, None);
        usage.record_turn(row.clone()).unwrap();
        usage
            .record_turn(UsageTurnRow {
                total_tokens: Some(99),
                ..row
            })
            .unwrap();
        let sums = usage.sums_since(0).unwrap();
        assert_eq!((sums[0].turns, sums[0].total_tokens), (1, 99));
    }

    #[test]
    fn a_sessions_turns_come_back_oldest_first_and_only_its_own() {
        let (_dir, storage) = storage();
        let usage = storage.usage();
        let mine = SessionId::new();
        for (name, at) in [("a", 1_000), ("b", 2_000), ("c", 3_000)] {
            usage
                .record_turn(UsageTurnRow {
                    session_id: mine,
                    ..turn(name, at, "omp", at, Some(0.5))
                })
                .unwrap();
        }
        usage
            .record_turn(turn("other", 1_500, "omp", 1, None))
            .unwrap();
        let rows = usage.turns_of(mine, 10).unwrap();
        let ids: Vec<_> = rows.iter().map(|r| r.turn_id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(rows[0].cost_usd, Some(0.5));
        assert_eq!(rows[2].input_tokens, Some(3_000));
        let newest: Vec<_> = usage
            .turns_of(mine, 2)
            .unwrap()
            .into_iter()
            .map(|r| r.turn_id)
            .collect();
        assert_eq!(newest, ["b", "c"], "the newest two, still oldest first");
    }

    fn limit(harness: &str, window: &str, used: f64) -> UsageLimitRow {
        UsageLimitRow {
            harness: harness.to_owned(),
            account: String::new(),
            provider: "openai".to_owned(),
            window_id: window.to_owned(),
            label: window.to_owned(),
            used_percent: used,
            window_minutes: Some(300),
            resets_at_ms: Some(9),
            plan: None,
            observed_at_ms: 1,
        }
    }

    #[test]
    fn limits_are_upserted_per_window_and_replaced_per_harness() {
        let (_dir, storage) = storage();
        let usage = storage.usage();
        usage
            .upsert_limits(vec![limit("codex", "5h", 10.0), limit("codex", "7d", 5.0)])
            .unwrap();
        usage
            .upsert_limits(vec![limit("codex", "5h", 80.0)])
            .unwrap();
        let rows = usage.limits().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter()
                .find(|r| r.window_id == "5h")
                .unwrap()
                .used_percent,
            80.0
        );
        usage.upsert_limits(vec![limit("omp", "5h", 1.0)]).unwrap();
        usage
            .replace_limits(
                "codex".to_owned(),
                String::new(),
                vec![limit("codex", "7d", 6.0)],
            )
            .unwrap();
        let rows = usage.limits().unwrap();
        let names: Vec<_> = rows
            .iter()
            .map(|r| (r.harness.as_str(), r.window_id.as_str()))
            .collect();
        assert_eq!(names, [("codex", "7d"), ("omp", "5h")]);
    }

    #[test]
    fn the_same_window_exists_once_per_account_and_replacing_one_leaves_the_others() {
        let (_dir, storage) = storage();
        let usage = storage.usage();
        let mut work = limit("claude", "5h", 40.0);
        work.account = "work".to_owned();
        let own = limit("claude", "5h", 10.0);
        usage.upsert_limits(vec![own, work.clone()]).unwrap();
        assert_eq!(usage.limits().unwrap().len(), 2);
        let mut updated = work;
        updated.used_percent = 90.0;
        usage.upsert_limits(vec![updated]).unwrap();
        let rows = usage.limits().unwrap();
        assert_eq!(rows.len(), 2);
        // Ordered by account: the harness's own login ('') first.
        assert_eq!((rows[0].account.as_str(), rows[0].used_percent), ("", 10.0));
        assert_eq!(
            (rows[1].account.as_str(), rows[1].used_percent),
            ("work", 90.0)
        );
        usage
            .replace_limits("claude".to_owned(), "work".to_owned(), Vec::new())
            .unwrap();
        let rows = usage.limits().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].account, "");
    }
}
