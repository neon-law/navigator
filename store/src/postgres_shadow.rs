//! [Shadow reads](../../docs/glossary/shadow-read.md): evidence that a
//! PostgreSQL read returns what the SurrealDB read it will replace returns.
//!
//! SurrealDB still answers every read. When a store is named in
//! [`STORES_ENV`], a sampled share of its covered reads ([`SAMPLE_ENV`]) also
//! runs the equivalent PostgreSQL query in a detached task, bounded by
//! [`SHADOW_TIMEOUT`]. Both results pass through the mirror's own row image,
//! so timestamps compare at PostgreSQL's microsecond precision and JSON
//! `null` reads as absent, exactly as `ops postgres mirror parity` compares
//! them. The caller's result, latency, and errors never depend on the
//! PostgreSQL side.
//!
//! Each comparison is counted per table ([`counts`]) and logged under the
//! [`LOG_TARGET`] target with the table, the row's UUID, and the verdict.
//! Neither the log nor the count ever carries a column value. The cutover
//! thresholds that turn these counts into a decision are in
//! `docs/postgres-tooling.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use uuid::Uuid;

use crate::postgres::PostgresDb;
use crate::postgres_mirror::MirrorError;

/// Comma-separated stores whose covered reads are shadowed, such as
/// `projects,firms`. Unset or empty turns shadow reads off.
pub const STORES_ENV: &str = "NAVIGATOR_POSTGRES_SHADOW_READS";

/// The share of covered reads to shadow, a decimal in `(0, 1]`. Defaults to
/// [`DEFAULT_SAMPLE`].
pub const SAMPLE_ENV: &str = "NAVIGATOR_POSTGRES_SHADOW_SAMPLE";

/// The share shadowed when [`SAMPLE_ENV`] is unset.
pub const DEFAULT_SAMPLE: f64 = 0.1;

/// The whole budget for one PostgreSQL comparison, connection included.
pub const SHADOW_TIMEOUT: Duration = Duration::from_secs(2);

/// The tracing target every shadow verdict is logged under.
pub const LOG_TARGET: &str = "navigator::postgres_shadow";

/// A store whose reads can be shadowed, named as in [`STORES_ENV`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShadowStore {
    /// `entities::find_by_id`.
    Entities,
    /// `firms::find_by_id`.
    Firms,
    /// `notations::find_by_id`.
    Notations,
    /// `projects::find_by_id` and `projects::find_by_code`.
    Projects,
    /// `inbox::conversation_by_id` (LAW-238 D0).
    InboxConversations,
    /// `inbox::message_by_id` (LAW-238 D0).
    InboxMessages,
}

impl ShadowStore {
    /// Every store.
    pub const ALL: [Self; 6] = [
        Self::Entities,
        Self::Firms,
        Self::Notations,
        Self::Projects,
        Self::InboxConversations,
        Self::InboxMessages,
    ];

    /// The store's name in [`STORES_ENV`].
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Entities => "entities",
            Self::Firms => "firms",
            Self::Notations => "notations",
            Self::Projects => "projects",
            Self::InboxConversations => "inbox_conversations",
            Self::InboxMessages => "inbox_messages",
        }
    }
}

/// A malformed shadow-read setting.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShadowConfigError {
    #[error(
        "{STORES_ENV} names an unknown store `{0}`; expected a comma-separated list of \
         entities, firms, notations, projects, inbox_conversations, inbox_messages"
    )]
    UnknownStore(String),
    #[error("{SAMPLE_ENV} must be a decimal greater than 0 and at most 1")]
    Sample,
}

/// Which stores are shadowed, and how often.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowConfig {
    stores: BTreeSet<ShadowStore>,
    sample: f64,
}

impl ShadowConfig {
    /// Shadow nothing.
    #[must_use]
    pub const fn off() -> Self {
        Self {
            stores: BTreeSet::new(),
            sample: DEFAULT_SAMPLE,
        }
    }

    /// Parse [`STORES_ENV`] and [`SAMPLE_ENV`].
    ///
    /// # Errors
    ///
    /// A store name outside [`ShadowStore::ALL`], or a sample that is not a
    /// finite decimal in `(0, 1]`.
    pub fn parse(stores: Option<&str>, sample: Option<&str>) -> Result<Self, ShadowConfigError> {
        let mut named = BTreeSet::new();
        for name in stores
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let store = ShadowStore::ALL
                .into_iter()
                .find(|store| store.name() == name)
                .ok_or_else(|| ShadowConfigError::UnknownStore(name.to_string()))?;
            named.insert(store);
        }
        let sample = match sample.map(str::trim).filter(|value| !value.is_empty()) {
            None => DEFAULT_SAMPLE,
            Some(value) => value
                .parse::<f64>()
                .ok()
                .filter(|rate| rate.is_finite() && *rate > 0.0 && *rate <= 1.0)
                .ok_or(ShadowConfigError::Sample)?,
        };
        Ok(Self {
            stores: named,
            sample,
        })
    }

    /// Whether `store`'s reads are shadowed at all.
    #[must_use]
    pub fn covers(&self, store: ShadowStore) -> bool {
        self.stores.contains(&store)
    }

    /// The configured share of covered reads.
    #[must_use]
    pub const fn sample(&self) -> f64 {
        self.sample
    }

    /// Whether a read of `store` whose uniform draw in `[0, 1)` is `draw` is
    /// shadowed.
    #[must_use]
    pub fn sampled(&self, store: ShadowStore, draw: f64) -> bool {
        self.covers(store) && draw < self.sample
    }
}

/// The process's settings, read once. A malformed value, or no
/// `NAVIGATOR_POSTGRES_HOST`, leaves shadow reads off; a malformed value is
/// logged once at error level, since a read must never fail on it.
pub fn config() -> &'static ShadowConfig {
    static CONFIG: OnceLock<ShadowConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let host = std::env::var("NAVIGATOR_POSTGRES_HOST").unwrap_or_default();
        let parsed = ShadowConfig::parse(
            std::env::var(STORES_ENV).ok().as_deref(),
            std::env::var(SAMPLE_ENV).ok().as_deref(),
        );
        match parsed {
            Ok(config) if !host.trim().is_empty() => config,
            Ok(_) => ShadowConfig::off(),
            Err(error) => {
                tracing::error!(target: LOG_TARGET, error = %error, "shadow reads are off");
                ShadowConfig::off()
            }
        }
    })
}

/// One PostgreSQL read judged against its SurrealDB result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Comparison {
    /// The row's UUID, from whichever store returned one.
    pub id: Option<Uuid>,
    /// Whether both stores returned the same normalized row, or both none.
    pub equal: bool,
}

/// The outcome of one shadow read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Both stores agree.
    Match,
    /// The stores disagree: a row is missing from one, or a column differs.
    Mismatch,
    /// PostgreSQL could not answer within [`SHADOW_TIMEOUT`], or the source
    /// row could not be normalized. Not evidence either way.
    Unavailable,
}

impl Verdict {
    /// The verdict as logged.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch => "mismatch",
            Self::Unavailable => "unavailable",
        }
    }
}

/// One table's shadow-read tally since the process started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShadowCounts {
    pub matched: u64,
    pub mismatched: u64,
    pub unavailable: u64,
}

fn tallies() -> &'static Mutex<BTreeMap<&'static str, ShadowCounts>> {
    static TALLIES: OnceLock<Mutex<BTreeMap<&'static str, ShadowCounts>>> = OnceLock::new();
    TALLIES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Every table's tally since the process started.
#[must_use]
pub fn counts() -> BTreeMap<&'static str, ShadowCounts> {
    tallies()
        .lock()
        .map(|tallies| tallies.clone())
        .unwrap_or_default()
}

fn record(table: &'static str, id: Option<Uuid>, verdict: Verdict) {
    if let Ok(mut tallies) = tallies().lock() {
        let tally = tallies.entry(table).or_default();
        match verdict {
            Verdict::Match => tally.matched += 1,
            Verdict::Mismatch => tally.mismatched += 1,
            Verdict::Unavailable => tally.unavailable += 1,
        }
    }
    let id = id.map_or_else(|| "none".to_string(), |id| id.to_string());
    let verdict_label = verdict.label();
    match verdict {
        Verdict::Match => {
            tracing::debug!(target: LOG_TARGET, table, id, verdict = verdict_label, "shadow read");
        }
        Verdict::Mismatch => tracing::warn!(
            target: LOG_TARGET,
            table,
            id,
            verdict = verdict_label,
            "shadow read differs from SurrealDB"
        ),
        Verdict::Unavailable => tracing::info!(
            target: LOG_TARGET,
            table,
            id,
            verdict = verdict_label,
            "shadow read could not compare"
        ),
    }
}

/// Judge one comparison within `timeout`, then count and log its verdict.
/// `id` names the row when the caller already knows it; otherwise the
/// comparison's own id is logged.
pub async fn judge<F>(
    table: &'static str,
    id: Option<Uuid>,
    timeout: Duration,
    comparison: F,
) -> Verdict
where
    F: Future<Output = Result<Comparison, MirrorError>>,
{
    let (verdict, id) = match tokio::time::timeout(timeout, comparison).await {
        Ok(Ok(Comparison { id: found, equal })) => (
            if equal {
                Verdict::Match
            } else {
                Verdict::Mismatch
            },
            id.or(found),
        ),
        Ok(Err(_)) | Err(_) => (Verdict::Unavailable, id),
    };
    record(table, id, verdict);
    verdict
}

/// Shadow one read of `store` under `config` when `draw` falls inside the
/// sample. Returns the detached task, or `None` when this read is not
/// shadowed or no Tokio runtime is running. `compare` receives the process's
/// shared PostgreSQL pool and is never called for an unsampled read.
pub fn observe_with<C, F>(
    config: &ShadowConfig,
    draw: f64,
    store: ShadowStore,
    table: &'static str,
    id: Option<Uuid>,
    compare: C,
) -> Option<tokio::task::JoinHandle<Verdict>>
where
    C: FnOnce(PostgresDb) -> F + Send + 'static,
    F: Future<Output = Result<Comparison, MirrorError>> + Send + 'static,
{
    if !config.sampled(store, draw) {
        return None;
    }
    let runtime = tokio::runtime::Handle::try_current().ok()?;
    Some(runtime.spawn(judge(table, id, SHADOW_TIMEOUT, async move {
        let pg = PostgresDb::from_env()
            .await
            .map_err(|_| MirrorError::Unavailable)?;
        compare(pg).await
    })))
}

/// Draw once for a read of `store`; `Some(draw)` when it is shadowed.
fn draw(store: ShadowStore) -> Option<f64> {
    let config = config();
    if !config.covers(store) {
        return None;
    }
    let draw = rand::random::<f64>();
    config.sampled(store, draw).then_some(draw)
}

/// Shadow a `firms::find_by_id` read.
pub(crate) fn firm_read(id: Uuid, result: Option<&crate::firms::Firm>) {
    let Some(draw) = draw(ShadowStore::Firms) else {
        return;
    };
    let source = result.cloned();
    let _ = observe_with(
        config(),
        draw,
        ShadowStore::Firms,
        "firm",
        Some(id),
        move |pg| async move {
            crate::postgres_mirror::tenancy::shadow_firm(&pg, id, source.as_ref()).await
        },
    );
}

/// Shadow a `projects::find_by_id` or `projects::find_by_code` read.
pub(crate) fn project_read(key: ShadowKey, result: Option<&crate::projects::Project>) {
    let Some(draw) = draw(ShadowStore::Projects) else {
        return;
    };
    let id = match &key {
        ShadowKey::Id(id) => Some(*id),
        ShadowKey::Text(_) => None,
    };
    let source = result.cloned();
    let _ = observe_with(
        config(),
        draw,
        ShadowStore::Projects,
        "project",
        id,
        move |pg| async move {
            crate::postgres_mirror::tenancy::shadow_project(&pg, key, source.as_ref()).await
        },
    );
}

/// Shadow an `entities::find_by_id` read.
pub(crate) fn entity_read(id: Uuid, result: Option<&crate::entities::Entity>) {
    let Some(draw) = draw(ShadowStore::Entities) else {
        return;
    };
    let source = result.cloned();
    let _ = observe_with(
        config(),
        draw,
        ShadowStore::Entities,
        "entity",
        Some(id),
        move |pg| async move {
            crate::postgres_mirror::identity::shadow_entity(&pg, id, source.as_ref()).await
        },
    );
}

/// Shadow a `notations::find_by_id` read.
pub(crate) fn notation_read(id: Uuid, result: Option<&crate::notations::Notation>) {
    let Some(draw) = draw(ShadowStore::Notations) else {
        return;
    };
    let source = result.cloned();
    let _ = observe_with(
        config(),
        draw,
        ShadowStore::Notations,
        "notation",
        Some(id),
        move |pg| async move {
            crate::postgres_mirror::workflow::shadow_notation(&pg, id, source.as_ref()).await
        },
    );
}

/// Shadow an `inbox::conversation_by_id` read (LAW-238 D0 cutover evidence).
pub(crate) fn conversation_read(id: Uuid, result: Option<&crate::inbox::Conversation>) {
    let Some(draw) = draw(ShadowStore::InboxConversations) else {
        return;
    };
    let source = result.cloned();
    let _ = observe_with(
        config(),
        draw,
        ShadowStore::InboxConversations,
        "inbox_conversation",
        Some(id),
        move |pg| async move {
            crate::postgres_mirror::inbox::shadow_conversation(&pg, id, source.as_ref()).await
        },
    );
}

/// Shadow an `inbox::message_by_id` read (LAW-238 D0 cutover evidence).
pub(crate) fn message_read(id: Uuid, result: Option<&crate::inbox::Message>) {
    let Some(draw) = draw(ShadowStore::InboxMessages) else {
        return;
    };
    let source = result.cloned();
    let _ = observe_with(
        config(),
        draw,
        ShadowStore::InboxMessages,
        "message",
        Some(id),
        move |pg| async move {
            crate::postgres_mirror::inbox::shadow_message(&pg, id, source.as_ref()).await
        },
    );
}

/// How a shadowed read found its row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShadowKey {
    /// By record UUID.
    Id(Uuid),
    /// By a unique text column, such as a Project code.
    Text(String),
}

/// Read the one PostgreSQL row `sql` selects for `key`, normalize it with
/// `read`, and compare it with the normalized SurrealDB `source`.
pub(crate) async fn compare_row<I: PartialEq>(
    pg: &PostgresDb,
    sql: &'static str,
    key: ShadowKey,
    source: Option<I>,
    read: fn(&sqlx::postgres::PgRow) -> Result<I, sqlx::Error>,
    id: fn(&I) -> Uuid,
) -> Result<Comparison, MirrorError> {
    let query = sqlx::query(sql);
    let query = match key {
        ShadowKey::Id(value) => query.bind(value),
        ShadowKey::Text(value) => query.bind(value),
    };
    let target = query
        .fetch_optional(&pg.0)
        .await?
        .as_ref()
        .map(read)
        .transpose()?;
    Ok(Comparison {
        id: source.as_ref().or(target.as_ref()).map(id),
        equal: source == target,
    })
}

#[cfg(test)]
#[path = "postgres_shadow_tests.rs"]
mod tests;
