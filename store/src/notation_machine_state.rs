//! Durable state for the Notation state machines on DBOS.
//!
//! The Restate virtual object held four keyed values per notation — the
//! questionnaire and workflow spec YAML plus each machine's current state.
//! DBOS has no virtual-object primitive, so the port moves those keys into
//! the `notation_machine_state` table: one row per
//! `(notation_id, machine_kind)`, read and written by the DBOS transition
//! workflow in `workflows-service::dbos_notation` while it holds the
//! notation's advisory lane ([`acquire_lane`]).
//!
//! Three invariants the port depends on:
//!
//! 1. **One serialized lane per notation.** [`acquire_lane`] takes a
//!    Postgres advisory lock keyed by the notation id; every transition body
//!    holds it for its whole duration, so questionnaire and workflow
//!    operations on one notation never interleave — the Restate virtual
//!    object's "one invocation at a time per key" guarantee, rebuilt on
//!    Postgres. A second holder waits up to [`LANE_LOCK_TIMEOUT`] before
//!    failing with [`MachineStateError::LaneTimeout`] (the DBOS step retries
//!    with its backoff); a crashed holder's lock dies with its connection.
//! 2. **Compare-and-set transitions.** [`apply_transition`] writes
//!    `current_state` only from the state the caller read. Under the lane
//!    that CAS is the crash-recovery check: a replayed workflow sees
//!    `AlreadyApplied` instead of double-applying.
//! 3. **Import never overwrites.** [`import`] backfills one row from the
//!    Restate Cloud state export and does nothing when the row already
//!    exists — once a machine lives here, Postgres, not the export, is its
//!    source of truth.
//!
//! Machine kinds are the lowercase tokens the journal and the wire contract
//! use — `questionnaire` and `workflow`
//! ([`crate::notation_events::MACHINE_QUESTIONNAIRE`] /
//! [`crate::notation_events::MACHINE_WORKFLOW`], also
//! `workflows::MachineKind::as_str`); the table's CHECK constraint is the
//! second guard behind the callers' typed `MachineKind`.

use serde::{Deserialize, Serialize};
use sqlx::{postgres::Postgres, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// One machine's durable state: the spec it runs on and the state
/// it is in — the two K/V values Restate keyed per machine, in one row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineState {
    pub notation_id: Uuid,
    pub machine_kind: String,
    pub spec_yaml: String,
    pub current_state: String,
    /// Questionnaire-and-workflow specs on ephemeral notations are test
    /// fixtures: mirrors `QuestionnaireSpec.ephemeral` so the port can keep
    /// skipping journal appends without re-deriving the flag.
    pub ephemeral: bool,
}

/// What [`apply_transition`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionOutcome {
    /// `from` matched; the state is now `next`.
    Applied,
    /// The state is already `next` — a recovery replay of a transition
    /// whose CAS landed before the crash.
    AlreadyApplied,
    /// Another transition moved the machine past `from`. The caller is a
    /// zombie replay; the machine is consistent and needs nothing from it.
    Conflicted { current: String },
}

/// What [`import`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportOutcome {
    /// The exported row was written.
    Imported,
    /// A row already existed and was left untouched.
    AlreadyPresent,
}

/// Errors reading or writing machine state, or acquiring a lane.
#[derive(Debug, Error)]
pub enum MachineStateError {
    /// A transition was attempted against a machine that never started.
    #[error("notation machine not started: {notation_id} {machine_kind}")]
    MachineMissing {
        notation_id: Uuid,
        machine_kind: String,
    },
    /// Another holder kept the notation's lane for [`LANE_LOCK_TIMEOUT`].
    /// Retryable — the DBOS step's backoff spaces the next attempt.
    #[error("notation lane is busy: {notation_id}")]
    LaneTimeout { notation_id: Uuid },
    #[error("database: {0}")]
    Database(String),
}

impl From<sqlx::Error> for MachineStateError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error.to_string())
    }
}

/// How long a transition waits for its notation's lane before failing.
const LANE_LOCK_TIMEOUT: &str = "5s";

/// The advisory-lock key for one notation's lane: a full-width FNV-1a hash
/// of all 16 id bytes. Truncating a UUIDv7 to its high bytes would key on
/// its millisecond timestamp — two notations created in the same
/// millisecond would collide constantly — so the whole id feeds the hash.
/// Advisory keys are 64-bit, so collisions remain possible in principle;
/// one would only briefly serialize two notations, never corrupt either.
#[must_use]
pub fn lane_key(notation_id: Uuid) -> i64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in notation_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    i64::from_be_bytes(hash.to_be_bytes())
}

/// Full read query for one machine's state row — static so sqlx's
/// `SqlSafeStr` audit holds it without an assertion.
const READ_SQL: &str = concat!(
    "SELECT ",
    "notation_id, machine_kind, spec_yaml, current_state, ephemeral \
     FROM notation_machine_state WHERE notation_id = $1 AND machine_kind = $2"
);

/// Start (or confirm) a machine: insert its spec at `BEGIN`, doing nothing
/// when the row already exists. Returns whether this call inserted it —
/// the port's parity for the Restate "if the spec is already stored the
/// start is a no-op" branch.
///
/// # Errors
///
/// [`MachineStateError`] for a database failure.
pub async fn start_machine(
    pg: &PostgresDb,
    notation_id: Uuid,
    machine_kind: &str,
    spec_yaml: &str,
    ephemeral: bool,
) -> Result<bool, MachineStateError> {
    let result = sqlx::query(
        "INSERT INTO notation_machine_state \
         (notation_id, machine_kind, spec_yaml, current_state, ephemeral) \
         VALUES ($1, $2, $3, 'BEGIN', $4) \
         ON CONFLICT (notation_id, machine_kind) DO NOTHING",
    )
    .bind(notation_id)
    .bind(machine_kind)
    .bind(spec_yaml)
    .bind(ephemeral)
    .execute(pg.pool())
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Read a machine's state row — `None` when the machine never started.
///
/// # Errors
///
/// [`MachineStateError`] for a database failure.
pub async fn read(
    pg: &PostgresDb,
    notation_id: Uuid,
    machine_kind: &str,
) -> Result<Option<MachineState>, MachineStateError> {
    let row = sqlx::query(READ_SQL)
        .bind(notation_id)
        .bind(machine_kind)
        .fetch_optional(pg.pool())
        .await?;
    Ok(row.map(|row| machine_state(&row)))
}

/// Move a machine from `from` to `next` only when it is still in `from`.
///
/// # Errors
///
/// [`MachineStateError::MachineMissing`] when the machine never started;
/// [`MachineStateError`] for a database failure.
pub async fn apply_transition(
    pg: &PostgresDb,
    notation_id: Uuid,
    machine_kind: &str,
    from: &str,
    next: &str,
) -> Result<TransitionOutcome, MachineStateError> {
    let result = sqlx::query(
        "UPDATE notation_machine_state SET current_state = $3, updated_at = now() \
         WHERE notation_id = $1 AND machine_kind = $2 AND current_state = $4",
    )
    .bind(notation_id)
    .bind(machine_kind)
    .bind(next)
    .bind(from)
    .execute(pg.pool())
    .await?;
    if result.rows_affected() > 0 {
        return Ok(TransitionOutcome::Applied);
    }
    let current =
        read(pg, notation_id, machine_kind)
            .await?
            .ok_or(MachineStateError::MachineMissing {
                notation_id,
                machine_kind: machine_kind.to_owned(),
            })?;
    if current.current_state == next {
        Ok(TransitionOutcome::AlreadyApplied)
    } else {
        Ok(TransitionOutcome::Conflicted {
            current: current.current_state,
        })
    }
}

/// Backfill one row from the Restate Cloud state export. Never overwrites:
/// an existing row means the machine already moved to Postgres.
///
/// # Errors
///
/// [`MachineStateError`] for a database failure.
pub async fn import(
    pg: &PostgresDb,
    notation_id: Uuid,
    machine_kind: &str,
    spec_yaml: &str,
    current_state: &str,
    ephemeral: bool,
) -> Result<ImportOutcome, MachineStateError> {
    let result = sqlx::query(
        "INSERT INTO notation_machine_state \
         (notation_id, machine_kind, spec_yaml, current_state, ephemeral) \
         VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (notation_id, machine_kind) DO NOTHING",
    )
    .bind(notation_id)
    .bind(machine_kind)
    .bind(spec_yaml)
    .bind(current_state)
    .bind(ephemeral)
    .execute(pg.pool())
    .await?;
    if result.rows_affected() > 0 {
        Ok(ImportOutcome::Imported)
    } else {
        Ok(ImportOutcome::AlreadyPresent)
    }
}

/// Hold one notation's lane for the duration of a transition body.
///
/// The guard pins one pooled connection in an open transaction that owns
/// the advisory lock; committing releases it, and dropping the guard rolls
/// the transaction back — which releases the lock even on a panic path, so
/// a lane can never be lost to a dead holder. The open transaction is the
/// cost of holding the lock across awaited steps: one of the pool's ten
/// connections per in-flight transition, human-paced in practice.
#[derive(Debug)]
pub struct LaneGuard {
    /// `None` once committed.
    transaction: Option<Transaction<'static, Postgres>>,
    notation_id: Uuid,
}

/// Take a notation's lane, waiting up to [`LANE_LOCK_TIMEOUT`] for the
/// current holder to finish.
///
/// # Errors
///
/// [`MachineStateError::LaneTimeout`] when the wait budget expires;
/// [`MachineStateError`] for a database failure.
pub async fn acquire_lane(
    pg: &PostgresDb,
    notation_id: Uuid,
) -> Result<LaneGuard, MachineStateError> {
    acquire_lane_with_timeout(pg, notation_id, LANE_LOCK_TIMEOUT).await
}

async fn acquire_lane_with_timeout(
    pg: &PostgresDb,
    notation_id: Uuid,
    lock_timeout: &str,
) -> Result<LaneGuard, MachineStateError> {
    let mut transaction = pg.pool().begin().await?;
    let result: Result<_, sqlx::Error> = async {
        // `SET LOCAL` cannot take a bind parameter, and the budget is a
        // fixed operator constant (or a test literal) — never user input —
        // so the dynamic string is audit-safe by construction.
        let setting = format!("SET LOCAL lock_timeout = '{lock_timeout}'");
        sqlx::query(sqlx::AssertSqlSafe(setting))
            .execute(&mut *transaction)
            .await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lane_key(notation_id))
            .execute(&mut *transaction)
            .await?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        // Dropping the transaction rolls back, releasing any partially
        // taken state; surface the wait-budget expiry by SQLSTATE so the
        // caller can retry the lane instead of failing the transition.
        if is_lock_timeout(&error) {
            return Err(MachineStateError::LaneTimeout { notation_id });
        }
        return Err(error.into());
    }
    Ok(LaneGuard {
        transaction: Some(transaction),
        notation_id,
    })
}

/// SQLSTATE `55P03` — `lock_not_available`, which is what PostgreSQL
/// reports when a `lock_timeout` cancels a waiting lock request.
fn is_lock_timeout(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database_error| database_error.code().as_deref() == Some("55P03"))
}

impl LaneGuard {
    /// Which notation this guard serializes.
    #[must_use]
    pub const fn notation_id(&self) -> Uuid {
        self.notation_id
    }

    /// Release the lane by committing the (empty) holder transaction.
    ///
    /// # Errors
    ///
    /// [`MachineStateError`] if the commit fails — the lock is released
    /// either way, so callers surface the error but never retry the lane.
    pub async fn commit(mut self) -> Result<(), MachineStateError> {
        let transaction = self
            .transaction
            .take()
            .expect("lane guard commits at most once");
        transaction.commit().await.map_err(MachineStateError::from)
    }
}

impl Drop for LaneGuard {
    fn drop(&mut self) {
        // An uncommitted guard rolls its transaction back on drop, which
        // releases the advisory lock — the panic-safe release path.
        if self.transaction.is_some() {
            tracing::debug!(
                notation_id = %self.notation_id,
                "notation lane guard dropped without commit — transaction rolled back",
            );
        }
    }
}

fn machine_state(row: &sqlx::postgres::PgRow) -> MachineState {
    MachineState {
        notation_id: row.get("notation_id"),
        machine_kind: row.get("machine_kind"),
        spec_yaml: row.get("spec_yaml"),
        current_state: row.get("current_state"),
        ephemeral: row.get("ephemeral"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_lane, acquire_lane_with_timeout, apply_transition, import, lane_key, read,
        start_machine, ImportOutcome, MachineStateError, TransitionOutcome,
    };
    use crate::postgres::PostgresDb;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    /// A disposable database on the test PostgreSQL server, migrated to
    /// head — the same fixture the workflows-service DBOS lane uses.
    async fn pg() -> Option<PostgresDb> {
        let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
            assert_ne!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
                Ok("1"),
                "required PostgreSQL test lacks its disposable port"
            );
            return None;
        };
        let port = port
            .parse::<u16>()
            .expect("test PostgreSQL port is numeric");
        let options = |database: &str| {
            PgConnectOptions::new()
                .host("127.0.0.1")
                .port(port)
                .username("postgres")
                .password("synthetic")
                .database(database)
                .ssl_mode(PgSslMode::Disable)
        };
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options("postgres"))
            .await
            .expect("connect to the test PostgreSQL server");
        // This identifier contains only a fixed prefix and UUID hex digits.
        let database = format!("notation_machine_state_{}", Uuid::now_v7().simple());
        let create = format!("CREATE DATABASE {database}");
        sqlx::query(sqlx::AssertSqlSafe(create))
            .execute(&admin)
            .await
            .expect("create the test database");
        admin.close().await;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options(&database))
            .await
            .expect("connect to the test database");
        let pg = PostgresDb(pool);
        pg.migrate_embedded(None)
            .await
            .expect("migrate the test database");
        Some(pg)
    }

    const SPEC: &str = "states:\n  BEGIN: {}\n";

    #[tokio::test]
    async fn start_is_idempotent_and_transitions_cas_through_the_machine() {
        let Some(pg) = pg().await else { return };
        let notation = Uuid::now_v7();

        assert!(start_machine(&pg, notation, "questionnaire", SPEC, false)
            .await
            .expect("start"));
        assert!(!start_machine(&pg, notation, "questionnaire", SPEC, false)
            .await
            .expect("re-start"));

        let state = read(&pg, notation, "questionnaire")
            .await
            .expect("read")
            .expect("machine row");
        assert_eq!(state.current_state, "BEGIN");
        assert_eq!(state.spec_yaml, SPEC);
        assert!(!state.ephemeral);

        let applied = apply_transition(&pg, notation, "questionnaire", "BEGIN", "END")
            .await
            .expect("apply");
        assert_eq!(applied, TransitionOutcome::Applied);

        // A replay that read the same `from` again sees its own write.
        let replay = apply_transition(&pg, notation, "questionnaire", "BEGIN", "END")
            .await
            .expect("replay");
        assert_eq!(replay, TransitionOutcome::AlreadyApplied);

        // A zombie replay from an older `from` is a no-op, not a rollback.
        let zombie = apply_transition(&pg, notation, "questionnaire", "BEGIN", "END")
            .await
            .expect("zombie");
        assert_eq!(zombie, TransitionOutcome::AlreadyApplied);

        let other = apply_transition(&pg, notation, "questionnaire", "BEGIN", "lawyer_review")
            .await
            .expect("conflict probe");
        assert_eq!(
            other,
            TransitionOutcome::Conflicted {
                current: "END".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn machines_are_isolated_by_kind_and_missing_starts_error() {
        let Some(pg) = pg().await else { return };
        let notation = Uuid::now_v7();

        start_machine(&pg, notation, "workflow", SPEC, false)
            .await
            .expect("start workflow machine");
        let questionnaire = read(&pg, notation, "questionnaire")
            .await
            .expect("read questionnaire");
        assert!(
            questionnaire.is_none(),
            "one machine's start must not start the other"
        );

        let missing = apply_transition(&pg, notation, "questionnaire", "BEGIN", "END")
            .await
            .expect_err("transitioning an unstarted machine errors");
        assert!(matches!(missing, MachineStateError::MachineMissing { .. }));
    }

    #[tokio::test]
    async fn import_backfills_and_never_overwrites() {
        let Some(pg) = pg().await else { return };
        let notation = Uuid::now_v7();

        let first = import(&pg, notation, "workflow", SPEC, "lawyer_review", false)
            .await
            .expect("import");
        assert_eq!(first, ImportOutcome::Imported);

        let state = read(&pg, notation, "workflow")
            .await
            .expect("read")
            .expect("row");
        assert_eq!(state.current_state, "lawyer_review");
        assert_eq!(state.spec_yaml, SPEC);

        // A second import of a *drifted* export must not clobber the row:
        // once the machine lives here, Postgres is its source of truth.
        let second = import(&pg, notation, "workflow", "states: {}\n", "BEGIN", true)
            .await
            .expect("re-import");
        assert_eq!(second, ImportOutcome::AlreadyPresent);
        let state = read(&pg, notation, "workflow")
            .await
            .expect("read")
            .expect("row");
        assert_eq!(state.current_state, "lawyer_review");
        assert_eq!(state.spec_yaml, SPEC);
        assert!(!state.ephemeral);
    }

    #[tokio::test]
    async fn lane_serializes_holders_and_times_out_bounded() {
        let Some(pg) = pg().await else { return };
        let notation = Uuid::now_v7();

        let lane = acquire_lane(&pg, notation).await.expect("acquire the lane");
        assert_eq!(lane.notation_id(), notation);

        // A second holder blocks until the first releases, then proceeds —
        // the virtual object's one-at-a-time guarantee, observed.
        let holder = tokio::spawn({
            let pg = pg.clone();
            async move {
                let started = Instant::now();
                let lane = acquire_lane(&pg, notation).await.expect("second holder");
                let waited = started.elapsed();
                lane.commit().await.expect("second holder commits");
                waited
            }
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !holder.is_finished(),
            "the second holder must still be waiting on the lane"
        );
        lane.commit().await.expect("first holder commits");
        let waited = holder.await.expect("second holder task");
        assert!(
            waited >= Duration::from_millis(200),
            "the second holder ran only after the first released: {waited:?}"
        );

        // A lane held across a full wait budget fails bounded, and the
        // failing acquisition leaves no lock behind.
        let blocker = acquire_lane(&pg, notation).await.expect("blocker");
        let expired = acquire_lane_with_timeout(&pg, notation, "50ms").await;
        assert!(
            matches!(expired, Err(MachineStateError::LaneTimeout { .. })),
            "a contended lane must fail on its wait budget: {expired:?}"
        );
        drop(blocker);

        let after = acquire_lane_with_timeout(&pg, notation, "50ms")
            .await
            .expect("lane is free again after the timed-out wait");
        after.commit().await.expect("release");
    }

    #[tokio::test]
    async fn dropping_a_lane_guard_releases_the_lock() {
        let Some(pg) = pg().await else { return };
        let notation = Uuid::now_v7();

        let lane = acquire_lane(&pg, notation).await.expect("acquire");
        drop(lane);

        let again = acquire_lane_with_timeout(&pg, notation, "50ms")
            .await
            .expect("the dropped guard's rollback released the lock");
        again.commit().await.expect("release");
    }

    #[test]
    fn lane_keys_are_deterministic_and_distinct_per_notation() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        assert_eq!(lane_key(a), lane_key(a), "the lane key is stable");
        assert_ne!(
            lane_key(a),
            lane_key(b),
            "distinct notations take distinct lanes"
        );
        // Same-millisecond v7 ids share their timestamp prefix; pin a fixed
        // pair so a truncating regression fails deterministically, not just
        // when the clock cooperates.
        let fixed_a = Uuid::from_u128(0x0197_1234_5678_0000_0000_0000_0000_0001);
        let fixed_b = Uuid::from_u128(0x0197_1234_5678_0000_0000_0000_0000_0002);
        assert_ne!(lane_key(fixed_a), lane_key(fixed_b));
    }
}
