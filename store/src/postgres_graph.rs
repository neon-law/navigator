//! The [Apache AGE relationship graph](postgres/migrations/20261031010000_create_navigator_graph.sql)
//! projection: a second engine's copy of the conflict graph, rebuilt from the
//! PostgreSQL mirrors and parity-checked against the Surreal traversal.
//!
//! Vertices load first, then edges, then indexes — the order a traversal
//! needs — in one transaction, so a run leaves either the old graph or the
//! new one. Vertex keys are the Surreal record UUIDs the mirrors carry, so a
//! `relationship.source_id` chains to the same row in both engines. Every
//! AGE call goes through the migration's loader functions: AGE's
//! three-argument `cypher()` accepts a bound parameter only as a bare
//! `Param` node, which the PL/pgSQL wrappers provide.
//!
//! After the projection, [`validate_graph`] checks the graph against its
//! sources (counts and edge endpoints), and [`compare_traversal`] runs the
//! same bounded walk on both engines — every conflict edge within
//! [`crate::conflicts::MAX_HOPS`] of a set of anchors — and reports the
//! difference. The backfill's graph arm ([`advance`]) records what fails, by
//! table and UUID only, in `mirror_backfill_failure` under the
//! `graph_projection` checkpoint, exactly as the relational groups do.
//!
//! The projection is a derived view of the mirrors, not a parity group: it
//! has no Surreal side to compare row by row, only traversals to compare, so
//! [`crate::postgres_backfill::plan`] does not list it. The graph is small by
//! construction — firm-scale rows, not ledger-scale — and each source table
//! is read in full, ordered, before it is batched into the loaders.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::conflicts::{self, NodeRef, ParityEdge};
use crate::postgres::{PostgresDb, PostgresError};
use crate::postgres_backfill::BackfillError;
use crate::surreal::SurrealDb;

/// The graph arm's checkpoint name in `mirror_backfill_checkpoint`.
pub(crate) const GRAPH_MIRROR: &str = "graph_projection";

/// Source rows per loader payload. The loader's `UNWIND` is one Cypher
/// statement per batch; this bounds a statement's size without making
/// thousands of round trips.
const LOAD_BATCH: usize = 500;

/// Anchors per parity pass, after sorting for determinism. A walk's cost is
/// its frontier, not its anchor list, and two hundred anchors cover every
/// entity in a firm's graph and most persons that hold a relationship.
const MAX_PARITY_ANCHORS: usize = 200;

/// Age labels hold one vertex per canonical table; the label is the table's
/// capitalized name, and the vertex key is the row's Surreal record UUID.
const VERTEX_KINDS: &[(&str, &str)] = &[
    ("person", "Person"),
    ("entity", "Entity"),
    ("project", "Project"),
    ("firm", "Firm"),
];

/// The edge types a source table backs, and the table each maps to.
/// `HANDLES` is derived — it has no source table and no count to compare.
const SOURCE_EDGE_KINDS: &[(&str, &str)] = &[
    ("HAS_ROLE", "entity_role"),
    ("RELATIONSHIP", "relationship"),
    ("WORKS_ON", "person_project_role"),
    ("MEMBER_OF", "person_firm_role"),
];

/// What a projection wrote, by vertex label and edge type. Counts only —
/// a report carries no graph content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Projection {
    /// Rows loaded per vertex label (`Person`, `Entity`, `Project`, `Firm`).
    pub vertices: BTreeMap<String, usize>,
    /// Edges loaded per edge type (`HAS_ROLE`, `RELATIONSHIP`, `WORKS_ON`,
    /// `MEMBER_OF`, `HANDLES`).
    pub edges: BTreeMap<String, usize>,
}

impl Projection {
    /// Every vertex and edge the projection wrote.
    #[must_use]
    pub fn total_rows(&self) -> usize {
        self.vertices.values().sum::<usize>() + self.edges.values().sum::<usize>()
    }
}

/// Why a graph projection step could not complete. Variants carry counts
/// and engine errors, never graph content.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// A mirror read or graph write failed.
    #[error("the graph projection could not read its sources or write the graph")]
    Store(#[from] sqlx::Error),
    /// A Cypher result could not be read back as the shared edge shape.
    #[error("the AGE graph traversal returned an unreadable result")]
    Traversal,
    /// The Surreal side of a traversal comparison failed.
    #[error("the Surreal conflict traversal failed")]
    SurrealTraversal,
}

/// Which store walks the relationship graph, read once per check from
/// `NAVIGATOR_GRAPH_AUTHORITY`.
///
/// `age` is the default: the graph tables' authoritative copy is
/// PostgreSQL's — the write path lands rows in `entity_role` and
/// `relationship` and their edges in the `navigator_graph` projection —
/// so the traversal reads the store that owns the data. `surreal` is the
/// rollback path, and it is honest about its limit: the Surreal graph
/// tables are retired from the write path, so they go stale the moment
/// the process writes anything. Set the flag to `surreal` to compare an
/// old answer or to finish an emergency rollback that also flips the
/// writes back, not for day-to-day truth. The flag exists so a bad
/// cutover is a one-line rollback, not a redeploy.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum GraphEngine {
    /// The original SurrealQL traversal over `<->entity_role` and
    /// `<->relationship`.
    Surreal,
    /// The `navigator_graph` projection in Apache AGE, in this module.
    #[default]
    Age,
}

/// The environment variable that selects the graph engine.
pub(crate) const GRAPH_AUTHORITY_ENV: &str = "NAVIGATOR_GRAPH_AUTHORITY";

/// Parse [`GRAPH_AUTHORITY_ENV`]'s value.
///
/// Accepts `surreal` and `age`, case-insensitively; anything else is an
/// error naming the accepted values — a typo must fail a boot or a test
/// loudly rather than silently run the wrong engine.
pub(crate) fn parse_graph_engine(value: &str) -> Result<GraphEngine, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "surreal" => Ok(GraphEngine::Surreal),
        "age" => Ok(GraphEngine::Age),
        other => Err(format!(
            "{GRAPH_AUTHORITY_ENV} must be `surreal` or `age`, got `{other}`"
        )),
    }
}

/// The engine this process runs the conflict graph on, resolved from the
/// environment once per call.
///
/// No variable set means [`GraphEngine::Age`]. An unusable value is an
/// error — see [`parse_graph_engine`].
pub(crate) fn graph_engine_from_env() -> Result<GraphEngine, String> {
    match std::env::var(GRAPH_AUTHORITY_ENV) {
        Ok(value) => parse_graph_engine(&value),
        Err(std::env::VarError::NotPresent) => Ok(GraphEngine::default()),
        Err(error) => Err(format!("read {GRAPH_AUTHORITY_ENV}: {error}")),
    }
}

/// The engine a graph read or write runs on: [`graph_engine_from_env`],
/// refined by the deployment's own shape.
///
/// A deployment that has not configured PostgreSQL at all runs the
/// Surreal engine — the same opt-in posture that keeps its whole web off
/// PostgreSQL until its configuration says otherwise, and the shape
/// every Surreal-only fixture boot has. When [`GraphEngine::Age`] is
/// selected and PostgreSQL is configured but unreachable, the call
/// fails: a graph write may not silently vanish, and a check may not
/// silently answer from a stale copy.
///
/// # Errors
///
/// [`GRAPH_AUTHORITY_ENV`]'s value is unusable, or the AGE engine needs
/// PostgreSQL and the connection fails.
pub(crate) async fn graph_engine() -> Result<GraphEngine, String> {
    match graph_engine_from_env()? {
        GraphEngine::Surreal => Ok(GraphEngine::Surreal),
        GraphEngine::Age => match PostgresDb::from_env().await {
            Ok(_) => Ok(GraphEngine::Age),
            Err(PostgresError::Configuration) => Ok(GraphEngine::Surreal),
            Err(error) => Err(format!(
                "the AGE graph engine needs PostgreSQL and it is unusable: {error}"
            )),
        },
    }
}

/// The runtime write path's vertex ensure: the graph gains the vertices
/// the one new edge needs, keyed by the Surreal record UUIDs, with the
/// name the display surfaces show. A name here wins; the function falls
/// back to the relational row's own name for a `None` — and to no name
/// for a row the relational table does not have, which the explanation
/// renders as the id until the next rebuild refreshes names.
///
/// The endpoint tables are the graph's own vertex kinds — `person` and
/// `entity` for every edge the runtime writes — passed by call sites
/// that compile them from [`crate::relationships::Endpoint`], never from
/// input.
///
/// # Errors
///
/// [`GraphError::Store`] if the ensure fails.
pub(crate) async fn ensure_vertices(
    pg: &PostgresDb,
    table: &str,
    rows: &[(Uuid, Option<String>)],
) -> Result<(), GraphError> {
    let payload = json!({"rows": rows
        .iter()
        .map(|(id, name)| json!({"id": id.to_string(), "name": name}))
        .collect::<Vec<_>>()})
    .to_string();
    sqlx::query(ENSURE_VERTICES_SQL)
        .bind(table)
        .bind(&payload)
        .execute(&pg.0)
        .await?;
    Ok(())
}

/// The runtime write path's one-edge load. The Surreal write this
/// mirrors was one `RELATE`, so one `CREATE` is the same semantics; the
/// next wholesale projection rebuild is the deduplicator of last
/// resort, and the conflict check reads edges by id, which a rebuild
/// renumbers anyway.
///
/// # Errors
///
/// [`GraphError::Store`] if the load fails.
pub(crate) async fn load_edge(
    pg: &PostgresDb,
    edge_kind: &str,
    payload: Value,
) -> Result<(), GraphError> {
    sqlx::query(LOAD_EDGES_SQL)
        .bind(edge_kind)
        .bind(json!({"rows": [payload]}).to_string())
        .execute(&pg.0)
        .await?;
    Ok(())
}

/// What the graph holds against its sources, after a projection. Counts
/// only: a validation never returns a row identifier.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphValidation {
    /// Graph vertex counts per label.
    pub vertices: BTreeMap<String, usize>,
    /// Source table row counts, keyed by the label each feeds.
    pub source_vertices: BTreeMap<String, usize>,
    /// Graph edge counts per type, including derived `HANDLES`.
    pub edges: BTreeMap<String, usize>,
    /// Source table row counts, keyed by the edge type each feeds.
    pub source_edges: BTreeMap<String, usize>,
    /// Edges whose start or end is not a vertex of the expected label.
    pub broken_endpoints: usize,
}

impl GraphValidation {
    /// Whether the graph and its sources agree: same vertex counts per
    /// label, same edge counts for every source-backed type, and no edge
    /// with a missing endpoint. Derived `HANDLES` has no source count to
    /// equal — its endpoint check still runs.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.vertices == self.source_vertices
            && SOURCE_EDGE_KINDS
                .iter()
                .all(|(kind, table)| self.edges.get(*kind) == self.source_edges.get(*table))
            && self.broken_endpoints == 0
    }
}

/// The bounded walk both engines are asked, and how far apart their
/// answers are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TraversalParity {
    /// Anchors both walks started from.
    pub anchors: usize,
    /// Distinct normalized edges the AGE walk returned.
    pub age_edges: usize,
    /// Distinct normalized edges the Surreal walk returned.
    pub surreal_edges: usize,
    /// Edges only the AGE walk reached.
    pub age_only: usize,
    /// Edges only the Surreal walk reached.
    pub surreal_only: usize,
}

impl TraversalParity {
    /// Whether both engines returned the same edge set. A graph with no
    /// anchors — no relationships or entity roles at all — is trivially
    /// clean on both sides.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.age_only == 0 && self.surreal_only == 0
    }
}

/// Rebuild the graph from the PostgreSQL mirrors: vertices, then edges,
/// then indexes, in one transaction so a failed run leaves the previous
/// graph standing. Indexes are idempotent and run after the commit — the
/// label tables do not exist until the first batch lands.
///
/// # Errors
///
/// A mirror read or graph write that fails rolls the transaction back and
/// surfaces as [`GraphError::Store`].
pub async fn project_graph(pg: &PostgresDb) -> Result<Projection, GraphError> {
    // The relationship source rows are read before the transaction: their
    // endpoint links feed both the loader payloads and the anchor list.
    let relationship = relationship_source_rows(pg).await?;
    let mut tx = pg.0.begin().await?;
    sqlx::query("SELECT * FROM graph_clear_navigator() AS t(v)")
        .execute(&mut *tx)
        .await?;

    let mut projection = Projection::default();
    for (table, label) in VERTEX_KINDS {
        let count = load_vertices(&mut tx, table).await?;
        projection.vertices.insert(label.to_string(), count);
    }
    projection.edges.insert(
        "HAS_ROLE".to_string(),
        load_edges(&mut tx, "has_role", &entity_role_rows(pg).await?).await?,
    );
    projection.edges.insert(
        "RELATIONSHIP".to_string(),
        load_edges(&mut tx, "relationship", &relationship.payloads).await?,
    );
    projection.edges.insert(
        "WORKS_ON".to_string(),
        load_edges(&mut tx, "works_on", &participation_rows(pg).await?).await?,
    );
    projection.edges.insert(
        "MEMBER_OF".to_string(),
        load_edges(&mut tx, "member_of", &membership_rows(pg).await?).await?,
    );
    projection.edges.insert(
        "HANDLES".to_string(),
        load_edges(&mut tx, "handles", &derived_handles(pg).await?).await?,
    );
    tx.commit().await?;

    create_indexes(pg).await?;
    Ok(projection)
}

/// The expression indexes the conflict traversal reads through. AGE stores
/// every property in one `agtype` map per label table, so an index targets
/// the expression, not a column. Created after the projection's commit,
/// when the label tables exist; rerunnable by construction.
///
/// # Errors
///
/// A database that cannot create the indexes.
pub async fn create_indexes(pg: &PostgresDb) -> Result<(), GraphError> {
    for statement in [
        "CREATE INDEX IF NOT EXISTS relationship_kind_idx \
         ON navigator_graph.\"RELATIONSHIP\" ((properties->>'kind'::text))",
        "CREATE INDEX IF NOT EXISTS relationship_confidence_idx \
         ON navigator_graph.\"RELATIONSHIP\" (((properties->>'confidence_pct'::text)::int))",
    ] {
        sqlx::query(statement).execute(&pg.0).await?;
    }
    Ok(())
}

/// Compare the graph against its sources: per-label vertex counts and
/// per-type edge counts, and every edge's endpoints present as vertices.
///
/// # Errors
///
/// A graph or source read that fails.
pub async fn validate_graph(pg: &PostgresDb) -> Result<GraphValidation, GraphError> {
    let mut validation = GraphValidation::default();
    for (table, label) in VERTEX_KINDS {
        let graph = vertex_count(pg, label).await?;
        let source = source_count(pg, table).await?;
        validation.vertices.insert(label.to_string(), graph);
        validation.source_vertices.insert(label.to_string(), source);
    }
    for (kind, table) in SOURCE_EDGE_KINDS {
        let graph = edge_count(pg, kind).await?;
        let source = source_count(pg, table).await?;
        validation.edges.insert(kind.to_string(), graph);
        validation.source_edges.insert(table.to_string(), source);
    }
    validation
        .edges
        .insert("HANDLES".to_string(), edge_count(pg, "HANDLES").await?);
    validation.broken_endpoints = broken_endpoints(pg).await?;
    Ok(validation)
}

/// Run the bounded conflict walk on both engines over the same anchors and
/// report the difference between their edge sets. The anchors are the
/// endpoints of the graph's own conflict edges, read from the PostgreSQL
/// mirrors, sorted and capped, so both engines walk from the same frontier.
///
/// # Errors
///
/// A store read or traversal that fails.
pub async fn compare_traversal(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<TraversalParity, GraphError> {
    let anchors = parity_anchors(pg).await?;
    let age = age_parity_edges(pg, &anchors).await?;
    let surreal_edges = conflicts::parity_edges(surreal, &anchors)
        .await
        .map_err(|_| GraphError::SurrealTraversal)?;
    let age_only = age.difference(&surreal_edges).count();
    let surreal_only = surreal_edges.difference(&age).count();
    Ok(TraversalParity {
        anchors: anchors.len(),
        age_edges: age.len(),
        surreal_edges: surreal_edges.len(),
        age_only,
        surreal_only,
    })
}

/// The backfill's graph arm: one checkpointed group that projects, validates,
/// and parity-checks the graph, recording what fails by table and UUID only.
/// Called from [`crate::postgres_backfill::advance`] after the relational
/// groups, so a run's report carries the graph checkpoint and failures like
/// any other group's.
///
/// # Errors
///
/// The error's Display plus its source chain: a recorded failure reason must
/// say why, not merely that something failed.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain.push_str(": ");
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    chain
}

/// A store read or write that fails propagates; graph-specific failures
/// (validation mismatches, traversal differences) are recorded as failure
/// rows and complete the group, so a run reports them instead of failing.
pub async fn advance(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    run_id: Uuid,
) -> Result<usize, BackfillError> {
    sqlx::query(
        "INSERT INTO mirror_backfill_checkpoint (run_id, mirror) VALUES ($1, $2) \
         ON CONFLICT (run_id, mirror) DO NOTHING",
    )
    .bind(run_id)
    .bind(GRAPH_MIRROR)
    .execute(&pg.0)
    .await?;
    let done: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar(
        "SELECT completed_at FROM mirror_backfill_checkpoint WHERE run_id = $1 AND mirror = $2",
    )
    .bind(run_id)
    .bind(GRAPH_MIRROR)
    .fetch_one(&pg.0)
    .await?;
    if done.is_some() {
        return Ok(0);
    }

    // A projection that still matches its sources is left standing: the
    // same drift contract the mirror groups follow, so a rerun with no
    // Surreal changes applies nothing and does not rewrite the graph.
    let mut failures: Vec<(&'static str, Option<String>, String)> = Vec::new();
    if graph_is_current(surreal, pg, &mut failures).await {
        complete_checkpoint(pg, run_id, 0, 0, failures).await?;
        return Ok(0);
    }
    // The pre-check's staleness findings are superseded by the rebuild's own
    // validation: discard them so a healthy rebuild does not inherit the
    // pre-rebuild state as failures.
    failures.clear();

    let mut applied = 0_usize;
    let mut batches = 0_usize;

    match project_graph(pg).await {
        Ok(projection) => {
            applied += projection.total_rows();
            batches += 1;
        }
        Err(error) => failures.push((
            "graph_projection",
            None,
            format!("the AGE graph projection failed: {}", error_chain(&error)),
        )),
    }

    match validate_graph(pg).await {
        Ok(validation) if validation.is_clean() => batches += 1,
        Ok(validation) => {
            batches += 1;
            failures.extend(validation_failures(&validation));
        }
        Err(error) => failures.push((
            "graph_validation",
            None,
            format!(
                "the AGE graph could not be validated: {}",
                error_chain(&error)
            ),
        )),
    }

    match compare_traversal(surreal, pg).await {
        Ok(parity) if parity.is_clean() => batches += 1,
        Ok(parity) => {
            batches += 1;
            failures.push((
                "graph_parity",
                None,
                format!(
                    "the AGE and Surreal conflict traversals disagree on {} anchors: \
                     {} AGE edges, {} Surreal edges, {} only in AGE, {} only in Surreal",
                    parity.anchors,
                    parity.age_edges,
                    parity.surreal_edges,
                    parity.age_only,
                    parity.surreal_only
                ),
            ));
        }
        Err(error) => failures.push((
            "graph_parity",
            None,
            format!("the AGE graph traversal could not be compared: {error}"),
        )),
    }

    complete_checkpoint(pg, run_id, batches, applied, failures).await?;
    Ok(batches)
}

/// Validation mismatches as failure rows: counts only, no identifiers.
fn validation_failures(
    validation: &GraphValidation,
) -> Vec<(&'static str, Option<String>, String)> {
    let mut failures = Vec::new();
    for (label, count) in &validation.vertices {
        let source = validation
            .source_vertices
            .get(label)
            .copied()
            .unwrap_or_default();
        if source != *count {
            failures.push((
                "graph_validation",
                None,
                format!("{label} vertices: {count} in the graph, {source} in the source table"),
            ));
        }
    }
    for (kind, table) in SOURCE_EDGE_KINDS {
        let source = validation
            .source_edges
            .get(*table)
            .copied()
            .unwrap_or_default();
        let count = validation.edges.get(*kind).copied().unwrap_or_default();
        if source != count {
            failures.push((
                "graph_validation",
                None,
                format!("{kind} edges: {count} in the graph, {source} in {table}"),
            ));
        }
    }
    if validation.broken_endpoints > 0 {
        failures.push((
            "graph_validation",
            None,
            format!(
                "{} graph edges have an endpoint that is not a vertex",
                validation.broken_endpoints
            ),
        ));
    }
    failures
}

/// Whether the standing projection still matches its sources: the count
/// and endpoint checks plus the Surreal traversal comparison, both clean.
/// Collected failures are appended for the checkpoint record.
async fn graph_is_current(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    failures: &mut Vec<(&'static str, Option<String>, String)>,
) -> bool {
    let mut current = true;
    match validate_graph(pg).await {
        Ok(validation) if validation.is_clean() => {}
        Ok(validation) => {
            current = false;
            failures.extend(validation_failures(&validation));
        }
        Err(error) => {
            current = false;
            failures.push((
                "graph_validation",
                None,
                format!(
                    "the standing AGE graph could not be validated: {}",
                    error_chain(&error)
                ),
            ));
        }
    }
    match compare_traversal(surreal, pg).await {
        Ok(parity) if parity.is_clean() => {}
        Ok(parity) => {
            current = false;
            failures.push((
                "graph_parity",
                None,
                format!(
                    "the standing AGE graph disagrees with the Surreal conflict traversal on \
                     {} anchors: {} AGE edges, {} Surreal edges, {} only in AGE, {} only in Surreal",
                    parity.anchors,
                    parity.age_edges,
                    parity.surreal_edges,
                    parity.age_only,
                    parity.surreal_only
                ),
            ));
        }
        Err(error) => {
            current = false;
            failures.push((
                "graph_parity",
                None,
                format!("the standing AGE graph traversal could not be compared: {error}"),
            ));
        }
    }
    current
}

/// Record the arm's outcome in its checkpoint and failure rows, in one
/// transaction — the same shape `complete_group` leaves relational groups in.
async fn complete_checkpoint(
    pg: &PostgresDb,
    run_id: Uuid,
    batches: usize,
    applied: usize,
    failures: Vec<(&'static str, Option<String>, String)>,
) -> Result<(), BackfillError> {
    let mut tx = pg.0.begin().await?;
    sqlx::query("DELETE FROM mirror_backfill_failure WHERE run_id = $1 AND mirror = $2")
        .bind(run_id)
        .bind(GRAPH_MIRROR)
        .execute(&mut *tx)
        .await?;
    for (table, row_id, reason) in &failures {
        sqlx::query(
            "INSERT INTO mirror_backfill_failure (id, run_id, mirror, row_table, row_id, reason) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::new_v4())
        .bind(run_id)
        .bind(GRAPH_MIRROR)
        .bind(table)
        .bind(row_id)
        .bind(reason)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query(
        "UPDATE mirror_backfill_checkpoint SET batches = batches + $3, applied = applied + $4, \
         failed = $5, completed_at = now(), updated_at = now() \
         WHERE run_id = $1 AND mirror = $2",
    )
    .bind(run_id)
    .bind(GRAPH_MIRROR)
    .bind(i32::try_from(batches).unwrap_or(i32::MAX))
    .bind(i64::try_from(applied).unwrap_or(i64::MAX))
    .bind(i32::try_from(failures.len()).unwrap_or(i32::MAX))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The loader wrappers, called by literal name. sqlx's `SqlSafeStr` gate
/// audits dynamically built SQL; these two statements are compile-time
/// constants, so they pass the gate by construction.
const LOAD_VERTICES_SQL: &str = "SELECT * FROM graph_load_vertices($1, $2) AS t(v)";
const LOAD_EDGES_SQL: &str = "SELECT * FROM graph_load_edges($1, $2) AS t(v)";

/// The runtime write path's vertex ensure: the graph gains at most the
/// endpoints of the edge being written, read back from the graph itself.
const ENSURE_VERTICES_SQL: &str = "SELECT graph_ensure_vertices($1, $2) AS created";

/// The one graph reader the parity pass calls, by literal name.
const CONFLICT_EDGES_SQL: &str = "SELECT t.v::text FROM graph_conflict_edges($1) AS t(v)";

/// Call one two-argument graph loader: the family kind and the payload
/// text, both bound. The helpers own their Cypher.
async fn load_batch(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sql: &'static str,
    kind: &str,
    payload: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(sql)
        .bind(kind)
        .bind(payload)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Call one single-argument graph reader and read its rows back as text —
/// `agtype` decodes as its JSON text; no custom type mapping.
async fn read_graph(
    pg: &PostgresDb,
    sql: &'static str,
    payload: &str,
) -> Result<Vec<String>, GraphError> {
    let rows = sqlx::query(sql)
        .bind(payload)
        .fetch_all(&pg.0)
        .await?
        .into_iter()
        .map(|row| row.try_get::<String, _>(0))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// The runtime conflict walk, by literal name.
const CONFLICT_WALK_SQL: &str = "SELECT t.v::text FROM graph_conflict_edges($1) AS t(v)";

/// One hop out of a frontier, by literal name.
const CONFLICT_FRONTIER_SQL: &str = "SELECT t.v::text FROM graph_conflict_frontier($1) AS t(v)";

/// One edge the runtime conflict walk returned, with what the scoring and
/// the explanations need: the normalization the Surreal traversal projects
/// (`TRAVERSAL_FIELDS`) — edge table, kind token, confidence — plus the
/// endpoint names that ride along in both engines' rows.
pub(crate) struct TraversalEdge {
    pub(crate) edge_table: String,
    pub(crate) from_table: String,
    pub(crate) from_id: String,
    pub(crate) from_name: Option<String>,
    pub(crate) to_table: String,
    pub(crate) to_id: String,
    pub(crate) to_name: Option<String>,
    pub(crate) kind: String,
    pub(crate) confidence_pct: i32,
}

/// The bounded conflict walk over `anchors`, read the way the runtime
/// `age` engine reads it: one call to the projection's
/// `graph_conflict_edges` — three per-hop rounds of
/// `graph_conflict_frontier` inside the function, the same bound the
/// Surreal walk's [`MAX_HOPS`] encodes. Each round re-matches earlier
/// rounds' edges (the anchors stay in the advancing frontier), so the
/// rows are deduplicated by edge id; the Surreal engine's
/// `array::distinct` does the same at its hop boundaries.
///
/// # Errors
///
/// [`GraphError::Traversal`] for a row that does not decode to a known
/// edge shape — the walk refuses to score what it cannot read.
pub(crate) async fn conflict_traversal(
    pg: &PostgresDb,
    anchors: &[NodeRef],
) -> Result<Vec<TraversalEdge>, GraphError> {
    if anchors.is_empty() {
        return Ok(Vec::new());
    }
    let anchor_list: Vec<Value> = anchors
        .iter()
        .map(|anchor| json!(anchor.id.to_string()))
        .collect();
    let rows = read_graph(pg, CONFLICT_WALK_SQL, &json!(anchor_list).to_string()).await?;
    let mut edges = Vec::with_capacity(rows.len());
    let mut seen_edge_ids = HashSet::new();
    for row in rows {
        let value: Value = serde_json::from_str(&row).map_err(|_| GraphError::Traversal)?;
        if let Some(edge_id) = value.get("edge_id").and_then(Value::as_i64) {
            if !seen_edge_ids.insert(edge_id) {
                continue;
            }
        }
        edges.push(traversal_edge(&value)?);
    }
    Ok(edges)
}

/// One hop out of `nodes` — the matter page's frontier read, restricted
/// to the given nodes by [`crate::conflicts::edges_among`].
///
/// # Errors
///
/// [`GraphError::Traversal`] for a row that does not decode.
pub(crate) async fn conflict_frontier_edges(
    pg: &PostgresDb,
    nodes: &[NodeRef],
) -> Result<Vec<TraversalEdge>, GraphError> {
    if nodes.is_empty() {
        return Ok(Vec::new());
    }
    let frontier: Vec<Value> = nodes
        .iter()
        .map(|node| json!(node.id.to_string()))
        .collect();
    let rows = read_graph(
        pg,
        CONFLICT_FRONTIER_SQL,
        &json!({"frontier": frontier}).to_string(),
    )
    .await?;
    let mut edges = Vec::with_capacity(rows.len());
    for row in rows {
        let value: Value = serde_json::from_str(&row).map_err(|_| GraphError::Traversal)?;
        edges.push(traversal_edge(&value)?);
    }
    Ok(edges)
}

/// One walk row (`agtype` JSON text) as a runtime traversal edge.
fn traversal_edge(value: &Value) -> Result<TraversalEdge, GraphError> {
    let edge_type = value
        .get("edge_type")
        .and_then(Value::as_str)
        .ok_or(GraphError::Traversal)?;
    let (edge_table, kind, confidence_pct) = normalized_kind(edge_type, value)?;
    let (from_table, from_id) = parity_endpoint(value.get("from_id"), value.get("from_label"))?;
    let (to_table, to_id) = parity_endpoint(value.get("to_id"), value.get("to_label"))?;
    Ok(TraversalEdge {
        edge_table,
        from_table,
        from_id,
        from_name: value
            .get("from_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        to_table,
        to_id,
        to_name: value
            .get("to_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        kind,
        confidence_pct,
    })
}

/// The edge-type match both readers share: the AGE edge label normalized
/// back to its edge table, the kind token the conflict check reads, and
/// the confidence (the implicit 100 a structural tie carries).
fn normalized_kind(edge_type: &str, value: &Value) -> Result<(String, String, i32), GraphError> {
    match edge_type {
        "HAS_ROLE" => Ok((
            "entity_role".to_string(),
            value
                .get("role")
                .and_then(Value::as_str)
                .ok_or(GraphError::Traversal)?
                .to_string(),
            100,
        )),
        "RELATIONSHIP" => Ok((
            "relationship".to_string(),
            value
                .get("kind")
                .and_then(Value::as_str)
                .ok_or(GraphError::Traversal)?
                .to_string(),
            value
                .get("confidence_pct")
                .and_then(Value::as_i64)
                .and_then(|pct| i32::try_from(pct).ok())
                .ok_or(GraphError::Traversal)?,
        )),
        _ => Err(GraphError::Traversal),
    }
}

/// Read one vertex source table and load it in batches. Rows with no name
/// load with a null property; a vertex must exist for the edges to find.
async fn load_vertices(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
) -> Result<usize, GraphError> {
    // Audited: the interpolated table name comes from the compile-time
    // `VERTEX_KINDS` table in this file, never from input.
    let statement = format!("SELECT id, name FROM {table} ORDER BY id");
    let rows = sqlx::query(sqlx::AssertSqlSafe(statement))
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .map(|row| {
            let id: Uuid = row.try_get("id")?;
            let name: Option<String> = row.try_get("name")?;
            Ok(json!({"id": id.to_string(), "name": name}))
        })
        .collect::<Result<Vec<Value>, sqlx::Error>>()?;
    for batch in rows.chunks(LOAD_BATCH) {
        load_batch(
            tx,
            LOAD_VERTICES_SQL,
            table,
            &json!({"rows": batch}).to_string(),
        )
        .await?;
    }
    Ok(rows.len())
}

/// Load one edge family's rows in batches, counting what was loaded.
async fn load_edges(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    rows: &[Value],
) -> Result<usize, GraphError> {
    for batch in rows.chunks(LOAD_BATCH) {
        load_batch(
            tx,
            LOAD_EDGES_SQL,
            kind,
            &json!({"rows": batch}).to_string(),
        )
        .await?;
    }
    Ok(rows.len())
}

/// The structural ties: a person's role at an entity. `role` is open-ended
/// by schema, so it rides the edge property, and its confidence is the
/// implicit 100 the conflict check reads it as.
async fn entity_role_rows(pg: &PostgresDb) -> Result<Vec<Value>, GraphError> {
    Ok(sqlx::query(
        "SELECT person_id, entity_id, role FROM entity_role ORDER BY person_id, entity_id",
    )
    .fetch_all(&pg.0)
    .await?
    .into_iter()
    .map(|row| {
        let person_id: Uuid = row.try_get("person_id")?;
        let entity_id: Uuid = row.try_get("entity_id")?;
        let role: String = row.try_get("role")?;
        Ok(json!({
            "from_id": person_id.to_string(),
            "to_id": entity_id.to_string(),
            "role": role,
        }))
    })
    .collect::<Result<Vec<Value>, sqlx::Error>>()?)
}

/// The typed relationships, as loader payloads plus the endpoint links the
/// anchors need. `in` and `out` are JSONB record links the mirror
/// normalized (`{"tb": "person", "id": {"Uuid": ...}}`); a link the mirror's
/// writer cannot produce is a read-side backstop: the row is reported with
/// a `__error` marker instead of becoming a graph edge.
async fn relationship_source_rows(pg: &PostgresDb) -> Result<RelationshipRows, GraphError> {
    let mut payloads = Vec::new();
    let mut endpoints = Vec::new();
    for row in sqlx::query(
        "SELECT id, \"in\"::text AS link_in, \"out\"::text AS link_out, kind, confidence_pct, \
         source_kind, source_id::text AS source_id, detail \
         FROM \"relationship\" ORDER BY id",
    )
    .fetch_all(&pg.0)
    .await?
    {
        let id: String = row.try_get("id")?;
        let link_in: String = row.try_get("link_in")?;
        let link_out: String = row.try_get("link_out")?;
        let (from_table, from_id) = record_link(&link_in);
        let (to_table, to_id) = record_link(&link_out);
        match (from_table, from_id, to_table, to_id) {
            (Some(from_table), Some(from_id), Some(to_table), Some(to_id)) => {
                let source_id: Option<String> = row.try_get::<Option<String>, _>("source_id")?;
                // The column is the Surreal record link the mirror wrote —
                // `{"tb": "relationship_log", "id": {"Uuid": ...}}` — so the
                // edge property carries the referenced row's UUID, the part
                // of the chain the graph can resolve. A bare JSON string
                // (a runtime-written row) passes through as itself.
                let source_id = source_id
                    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                    .and_then(|value| match &value {
                        Value::Object(_) => value
                            .get("id")
                            .and_then(|id| id.get("Uuid"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        Value::String(text) => Some(text.clone()),
                        _ => None,
                    });
                endpoints.push((from_table.clone(), from_id.clone()));
                endpoints.push((to_table.clone(), to_id.clone()));
                payloads.push(json!({
                    "from_id": from_id,
                    "to_id": to_id,
                    "kind": row.try_get::<String, _>("kind")?,
                    "confidence_pct": row.try_get::<i64, _>("confidence_pct")?,
                    "source_kind": row.try_get::<String, _>("source_kind")?,
                    "source_id": source_id,
                    "detail": row.try_get::<Option<String>, _>("detail")?,
                }));
            }
            _ => {
                payloads.push(json!({
                    "__error": format!("relationship {id} has a malformed endpoint link"),
                }));
            }
        }
    }
    Ok(RelationshipRows {
        payloads,
        endpoints,
    })
}

/// The typed relationships' loader payloads and their endpoint links.
struct RelationshipRows {
    payloads: Vec<Value>,
    endpoints: Vec<(String, String)>,
}

/// A mirror record link (`{"tb": "person", "id": {"Uuid": ...}}`) as text,
/// decoded to its table and UUID. `None` for anything else — the mirror's
/// writer cannot produce one, so this is a read-side backstop.
fn record_link(link: &str) -> (Option<String>, Option<String>) {
    let value = serde_json::from_str::<Value>(link).ok();
    let table = value
        .as_ref()
        .and_then(|value| value.get("tb"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let id = value
        .as_ref()
        .and_then(|value| value.get("id"))
        .and_then(|id| id.get("Uuid"))
        .and_then(Value::as_str)
        .map(str::to_string);
    (table, id)
}

/// Participation: a person's role on a project, as `WORKS_ON`.
async fn participation_rows(pg: &PostgresDb) -> Result<Vec<Value>, GraphError> {
    Ok(sqlx::query(
        "SELECT person_id, project_id, participation FROM person_project_role \
         ORDER BY person_id, project_id",
    )
    .fetch_all(&pg.0)
    .await?
    .into_iter()
    .map(|row| {
        let person_id: Uuid = row.try_get("person_id")?;
        let project_id: Uuid = row.try_get("project_id")?;
        let participation: String = row.try_get("participation")?;
        Ok(json!({
            "from_id": person_id.to_string(),
            "to_id": project_id.to_string(),
            "role": participation,
        }))
    })
    .collect::<Result<Vec<Value>, sqlx::Error>>()?)
}

/// Membership: a person's firm role, as `MEMBER_OF`.
async fn membership_rows(pg: &PostgresDb) -> Result<Vec<Value>, GraphError> {
    Ok(
        sqlx::query("SELECT person_id, firm_id FROM person_firm_role ORDER BY person_id, firm_id")
            .fetch_all(&pg.0)
            .await?
            .into_iter()
            .map(|row| {
                let person_id: Uuid = row.try_get("person_id")?;
                let firm_id: Uuid = row.try_get("firm_id")?;
                Ok(json!({
                    "from_id": person_id.to_string(),
                    "to_id": firm_id.to_string(),
                }))
            })
            .collect::<Result<Vec<Value>, sqlx::Error>>()?,
    )
}

/// The one derivation: a firm handles every project any of its members work
/// on. An aggregate of the two participation tables, not a source of its own.
async fn derived_handles(pg: &PostgresDb) -> Result<Vec<Value>, GraphError> {
    Ok(sqlx::query(
        "SELECT DISTINCT pfr.firm_id, ppr.project_id \
         FROM person_firm_role pfr \
         JOIN person_project_role ppr ON ppr.person_id = pfr.person_id \
         ORDER BY 1, 2",
    )
    .fetch_all(&pg.0)
    .await?
    .into_iter()
    .map(|row| {
        let firm_id: Uuid = row.try_get(0)?;
        let project_id: Uuid = row.try_get(1)?;
        Ok(json!({
            "from_id": firm_id.to_string(),
            "to_id": project_id.to_string(),
        }))
    })
    .collect::<Result<Vec<Value>, sqlx::Error>>()?)
}

/// One label's vertex count in the graph.
async fn vertex_count(pg: &PostgresDb, label: &str) -> Result<usize, GraphError> {
    // Audited: the quoted label comes from the compile-time `VERTEX_KINDS`
    // table in this file, never from input.
    let statement = format!("SELECT count(*) FROM navigator_graph.\"{label}\"");
    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
        .fetch_one(&pg.0)
        .await?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

/// One type's edge count in the graph.
async fn edge_count(pg: &PostgresDb, kind: &str) -> Result<usize, GraphError> {
    // Audited: the quoted edge type comes from the compile-time
    // `SOURCE_EDGE_KINDS` table in this file, never from input.
    let statement = format!("SELECT count(*) FROM navigator_graph.\"{kind}\"");
    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
        .fetch_one(&pg.0)
        .await?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

/// One source table's row count.
async fn source_count(pg: &PostgresDb, table: &str) -> Result<usize, GraphError> {
    // Audited: the quoted table name comes from the compile-time source
    // tables in this file, never from input.
    let statement = format!("SELECT count(*) FROM \"{table}\"");
    let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
        .fetch_one(&pg.0)
        .await?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

/// Edges whose start or end is not a vertex of the label the edge type
/// connects, summed over the edge types. An edge the loaders wrote always
/// has both — this catches a graph a later migration or hand edit broke.
async fn broken_endpoints(pg: &PostgresDb) -> Result<usize, GraphError> {
    let mut broken = 0_usize;
    for (kind, from_labels, to_labels) in [
        ("HAS_ROLE", &["Person"][..], &["Entity"][..]),
        (
            "RELATIONSHIP",
            &["Person", "Entity"][..],
            &["Person", "Entity"][..],
        ),
        ("WORKS_ON", &["Person"][..], &["Project"][..]),
        ("MEMBER_OF", &["Person"][..], &["Firm"][..]),
        ("HANDLES", &["Firm"][..], &["Project"][..]),
    ] {
        let present_start = from_labels
            .iter()
            .map(|label| {
                format!("SELECT 1 FROM navigator_graph.\"{label}\" v WHERE v.id = e.start_id")
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let present_end = to_labels
            .iter()
            .map(|label| {
                format!("SELECT 1 FROM navigator_graph.\"{label}\" v WHERE v.id = e.end_id")
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let statement = format!(
            "SELECT count(*) FROM navigator_graph.\"{kind}\" e \
             WHERE NOT EXISTS ({present_start}) OR NOT EXISTS ({present_end})"
        );
        // Audited: the labels, edge types, and structure come from the
        // compile-time endpoint table in this function, never from input.
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(statement))
            .fetch_one(&pg.0)
            .await?;
        broken += usize::try_from(count).unwrap_or(0);
    }
    Ok(broken)
}

/// The anchors a parity pass walks from: every person or entity that holds
/// a conflict edge, read from the PostgreSQL mirrors, sorted and capped.
/// A `relationship` row whose links cannot be read is skipped — the
/// projection reports it; parity walks the graph that was built.
async fn parity_anchors(pg: &PostgresDb) -> Result<Vec<NodeRef>, GraphError> {
    let relationship = relationship_source_rows(pg).await?;
    let mut anchors: BTreeSet<(String, String)> = relationship.endpoints.into_iter().collect();
    for row in sqlx::query("SELECT person_id, entity_id FROM entity_role")
        .fetch_all(&pg.0)
        .await?
    {
        let person_id: Uuid = row.try_get("person_id")?;
        let entity_id: Uuid = row.try_get("entity_id")?;
        anchors.insert((String::from("person"), person_id.to_string()));
        anchors.insert((String::from("entity"), entity_id.to_string()));
    }
    let node_refs = anchors
        .into_iter()
        .filter_map(|(table, id)| {
            let id = Uuid::parse_str(&id).ok()?;
            match table.as_str() {
                "person" => Some(NodeRef::person(id)),
                "entity" => Some(NodeRef::entity(id)),
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    let mut node_refs = node_refs;
    node_refs.truncate(MAX_PARITY_ANCHORS);
    Ok(node_refs)
}

/// The AGE side of the parity comparison: the bounded conflict walk over the
/// anchors, read back as text and normalized to the shared edge shape.
async fn age_parity_edges(
    pg: &PostgresDb,
    anchors: &[NodeRef],
) -> Result<BTreeSet<ParityEdge>, GraphError> {
    let anchor_list: Vec<Value> = anchors
        .iter()
        .map(|anchor| json!(anchor.id.to_string()))
        .collect();
    let rows = read_graph(pg, CONFLICT_EDGES_SQL, &json!(anchor_list).to_string()).await?;
    let mut edges = BTreeSet::new();
    for row in rows {
        let value: Value = serde_json::from_str(&row).map_err(|_| GraphError::Traversal)?;
        edges.insert(normalize_age_edge(&value)?);
    }
    Ok(edges)
}

/// One AGE walk row (`agtype` JSON text) as the shared edge shape.
fn normalize_age_edge(value: &Value) -> Result<ParityEdge, GraphError> {
    let edge_type = value
        .get("edge_type")
        .and_then(Value::as_str)
        .ok_or(GraphError::Traversal)?;
    let (edge_table, kind, confidence_pct) = normalized_kind(edge_type, value)?;
    let (from_table, from_id) = parity_endpoint(value.get("from_id"), value.get("from_label"))?;
    let (to_table, to_id) = parity_endpoint(value.get("to_id"), value.get("to_label"))?;
    Ok(ParityEdge {
        edge_table,
        from_table,
        from_id,
        to_table,
        to_id,
        kind,
        confidence_pct,
    })
}

/// One walk endpoint as the shared shape: the AGE label (`Person`) mapped
/// back to its table (`person`), the id already the Surreal record UUID.
fn parity_endpoint(
    id: Option<&Value>,
    label: Option<&Value>,
) -> Result<(String, String), GraphError> {
    let id = id
        .and_then(Value::as_str)
        .ok_or(GraphError::Traversal)?
        .to_string();
    let table = match label.and_then(Value::as_str) {
        Some("Person") => "person",
        Some("Entity") => "entity",
        _ => return Err(GraphError::Traversal),
    };
    Ok((table.to_string(), id))
}

#[cfg(test)]
#[path = "postgres_graph_tests.rs"]
mod postgres_graph_tests;
