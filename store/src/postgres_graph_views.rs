//! Read-only graph views over the AGE projection: the neighborhood shapes
//! behind the graph REST endpoints. The Cypher lives in the PL/pgSQL reader
//! functions (`20261031020000_create_graph_view_readers.sql`); this module
//! calls them by literal name with bound payloads, decodes the `agtype`
//! rows as JSON text, and assembles `{nodes, edges}` views.
//!
//! All three views are read-only over the projection — authorization happens
//! before these functions run (portal handlers scope the seed by firm_id and
//! participation), and the traversal itself carries no tenancy filter: the
//! graph is global, the view is not.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::postgres::PostgresDb;
use crate::postgres_graph::GraphError;
use sqlx::Row;

/// The server-side hop ceiling shared by every view. The REST layer rejects
/// anything above it; the store clamps below it defensively.
pub const MAX_GRAPH_DEPTH: u8 = 3;

/// One node of a rendered graph view. `label` is the AGE vertex label
/// (`Person`, `Entity`, `Project`, `Firm`); `kind` restates it for the
/// renderer (the two always agree today — the renderer keys styling off
/// `kind`, and a future vertex with a subdivided label would map several
/// labels onto one kind).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GraphNode {
    pub id: String,
    pub label: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One edge of a rendered graph view, endpoints identified by node id.
/// `edge_type` is the AGE edge label; `kind`/`confidence_pct`/`source_kind`
/// carry the relationship provenance (see the conflict readers), `role`
/// carries the open-ended role value of `HAS_ROLE` and `WORKS_ON` edges.
/// `id` is the stored AGE edge identity — deduplication key, not wire shape.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GraphEdge {
    pub source: String,
    pub target: String,
    #[serde(rename = "type")]
    pub edge_type: String,
    #[serde(skip)]
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence_pct: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
}

/// The `FIRM_TIE` edge shape: an aggregated firm-to-firm edge. `strength`
/// counts the distinct person-level edges (or shared matters) behind the tie.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FirmEdge {
    pub source: String,
    pub target: String,
    #[serde(rename = "type")]
    pub edge_type: String,
    pub kind: String,
    pub strength: i64,
}

/// One rendered graph view: the neighborhood and its edges. Nodes and edges
/// come back ordered by id for stable pagination-friendly rendering.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct GraphView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

/// The firm view's payload: the seed firm, its counterpart firms, and the
/// aggregated ties between them.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct FirmView {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<FirmEdge>,
}

/// One vertex as the reader functions return it.
#[derive(Debug, Deserialize)]
struct SeedVertex {
    id: String,
    label: String,
    name: Option<String>,
}

/// One edge row as the reader functions return it — the same shape the
/// conflict readers use (`from_*`/`to_*` are canonical start/end of the
/// stored edge, not traversal direction).
#[derive(Debug, Deserialize)]
struct EdgeRow {
    edge_id: Value,
    edge_type: String,
    kind: Option<String>,
    role: Option<String>,
    confidence_pct: Option<i64>,
    source_kind: Option<String>,
    from_id: String,
    from_label: String,
    to_id: String,
    to_label: String,
    from_name: Option<String>,
    to_name: Option<String>,
}

/// A counterpart-firm tie as the firm-view arm returns it.
#[derive(Debug, Deserialize)]
struct FirmTie {
    other_firm_id: String,
    other_firm_name: Option<String>,
    kind: String,
    strength: i64,
}

const SEED_SQL: &str = "SELECT t.v::text FROM graph_view_seed($1) AS t(v)";
const PROJECT_ATTACH_SQL: &str = "SELECT t.v::text FROM graph_project_attach($1) AS t(v)";
const PROJECT_EXPAND_SQL: &str = "SELECT t.v::text FROM graph_project_expand($1) AS t(v)";
const LAWYER_EXPAND_SQL: &str = "SELECT t.v::text FROM graph_lawyer_expand($1) AS t(v)";
const FIRM_VIEW_SQL: &str = "SELECT t.v::text FROM graph_firm_view($1) AS t(v)";

/// The project view: the matter's parties, their entity roles, the
/// adverse/related relationships, and the firms handling each side — the
/// conflict check, made visible. Depth bounds the frontier walk (1 = the
/// project's own attachments only).
pub async fn project_view(
    pg: &PostgresDb,
    project_id: Uuid,
    depth: u8,
) -> Result<GraphView, GraphError> {
    let seed = project_id.to_string();
    let Some(seed_vertex) = seed_vertex(pg, &seed).await? else {
        // The projection has not been run yet: an empty neighborhood, not an
        // error — the caller renders the shell.
        return Ok(GraphView::default());
    };
    let mut nodes = node_map([seed_vertex]);
    let mut edges = BTreeMap::new();

    // Hop 1: everything attached to the project itself (parties, handling
    // firms). Later hops walk outward from whoever is newly reached.
    let attach_rows =
        read_graph(pg, PROJECT_ATTACH_SQL, &format!("{{\"seed\": \"{seed}\"}}")).await?;
    let mut frontier = Vec::new();
    for row in attach_rows {
        let raw: EdgeRow = decode(&row, "graph view edge")?;
        frontier.extend(new_endpoints(&raw, &mut nodes));
        collect_edge(&mut edges, edge_from_row(&raw)?);
    }

    // Hops 2+: only persons expand — relationships and entity roles anchor
    // on persons, and a firm's participation is already materialized as its
    // HANDLES edges.
    for _ in 1..depth.min(MAX_GRAPH_DEPTH) {
        if frontier.is_empty() {
            break;
        }
        let rows = read_graph(pg, PROJECT_EXPAND_SQL, &frontier_payload(&frontier)).await?;
        frontier = Vec::new();
        for row in rows {
            let raw: EdgeRow = decode(&row, "graph view edge")?;
            frontier.extend(new_endpoints(&raw, &mut nodes));
            collect_edge(&mut edges, edge_from_row(&raw)?);
        }
    }

    Ok(assemble_view(nodes, edges))
}

/// The lawyer view: matters worked on, entity roles, firm membership, and the
/// client/adverse relationships around one person.
pub async fn lawyer_view(
    pg: &PostgresDb,
    person_id: Uuid,
    depth: u8,
) -> Result<GraphView, GraphError> {
    let seed = person_id.to_string();
    let Some(seed_vertex) = seed_vertex(pg, &seed).await? else {
        return Ok(GraphView::default());
    };
    let mut nodes = node_map([seed_vertex]);
    let mut edges = BTreeMap::new();

    let mut frontier = vec![seed];
    for _ in 0..depth.min(MAX_GRAPH_DEPTH) {
        if frontier.is_empty() {
            break;
        }
        let rows = read_graph(pg, LAWYER_EXPAND_SQL, &frontier_payload(&frontier)).await?;
        frontier = Vec::new();
        for row in rows {
            let raw: EdgeRow = decode(&row, "graph view edge")?;
            frontier.extend(new_endpoints(&raw, &mut nodes));
            collect_edge(&mut edges, edge_from_row(&raw)?);
        }
    }

    Ok(assemble_view(nodes, edges))
}

/// The firm view: firm-to-firm ties aggregated up from person-level edges —
/// adversary, co-counsel, of-counsel, and client. Every counterpart firm sits
/// exactly three hops away (member -> person-level edge -> member's firm), so
/// the read is one aggregation query, independent of `depth`.
pub async fn firm_view(pg: &PostgresDb, firm_id: Uuid) -> Result<FirmView, GraphError> {
    let seed = firm_id.to_string();
    let Some(seed_vertex) = seed_vertex(pg, &seed).await? else {
        return Ok(FirmView::default());
    };
    let seed_node = seed_vertex_to_node(&seed_vertex);
    let mut nodes = vec![seed_node];
    let mut edges = BTreeMap::new();

    let rows = read_graph(pg, FIRM_VIEW_SQL, &format!("{{\"seed\": \"{seed}\"}}")).await?;
    let mut counterparts = BTreeMap::new();
    for row in rows {
        let tie: FirmTie = decode(&row, "firm tie")?;
        // Aggregate arms can report the same (counterpart, kind) twice —
        // the co-counsel arm counts shared matters, the relationship arms
        // count person edges — so same-kind rows sum and the last name wins.
        let entry = edges
            .entry((tie.other_firm_id.clone(), tie.kind.clone()))
            .or_insert_with(|| FirmEdge {
                source: seed.clone(),
                target: tie.other_firm_id.clone(),
                edge_type: "FIRM_TIE".to_string(),
                kind: tie.kind.clone(),
                strength: 0,
            });
        entry.strength += tie.strength;
        counterparts.insert(
            tie.other_firm_id.clone(),
            GraphNode {
                id: tie.other_firm_id,
                label: "Firm".to_string(),
                kind: "Firm".to_string(),
                name: tie.other_firm_name,
            },
        );
    }

    nodes.extend(counterparts.into_values());
    Ok(FirmView {
        nodes,
        edges: edges.into_values().collect(),
    })
}

/// The seed vertex, or `None` when the projection has not run yet.
async fn seed_vertex(pg: &PostgresDb, seed: &str) -> Result<Option<SeedVertex>, GraphError> {
    let rows = read_graph(pg, SEED_SQL, &format!("{{\"seed\": \"{seed}\"}}")).await?;
    match rows.first() {
        Some(text) => Ok(Some(decode(text, "seed vertex")?)),
        None => Ok(None),
    }
}

/// Read one single-argument reader and return its `agtype` rows as text —
/// `agtype` decodes as its JSON text; no custom type mapping. The same
/// pattern as `postgres_graph::read_graph`.
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

/// Decode one `agtype` row of JSON text.
fn decode<T: for<'de> Deserialize<'de>>(text: &str, what: &str) -> Result<T, GraphError> {
    serde_json::from_str(text)
        .map_err(|_| GraphError::Traversal)
        .inspect_err(|_| {
            tracing::warn!(what, row = text, "unreadable agtype row in a graph view");
        })
}

fn seed_vertex_to_node(vertex: &SeedVertex) -> GraphNode {
    GraphNode {
        id: vertex.id.clone(),
        label: vertex.label.clone(),
        kind: vertex.label.clone(),
        name: vertex.name.clone(),
    }
}

fn node_map(vertices: impl IntoIterator<Item = SeedVertex>) -> BTreeMap<String, GraphNode> {
    vertices
        .into_iter()
        .map(|vertex| (vertex.id.clone(), seed_vertex_to_node(&vertex)))
        .collect()
}

/// Endpoints of `edge` the view has not reached yet — the next frontier.
fn new_endpoints(edge: &EdgeRow, nodes: &mut BTreeMap<String, GraphNode>) -> Vec<String> {
    let mut next = Vec::new();
    for (id, label, name) in [
        (&edge.from_id, &edge.from_label, &edge.from_name),
        (&edge.to_id, &edge.to_label, &edge.to_name),
    ] {
        if !nodes.contains_key(id) {
            nodes.insert(
                id.clone(),
                GraphNode {
                    id: id.clone(),
                    label: label.clone(),
                    kind: label.clone(),
                    name: name.clone(),
                },
            );
            next.push(id.clone());
        }
    }
    next
}

/// Key each edge by its stored identity so the same graph edge discovered
/// from two frontier nodes lands once.
fn collect_edge(edges: &mut BTreeMap<String, GraphEdge>, edge: GraphEdge) {
    edges.insert(format!("{}:{}", edge.edge_type, edge.id), edge);
}

/// Normalize one decoded reader row into the rendered edge shape. `kind` is
/// the relationship kind; `role` is the open-ended role of `HAS_ROLE`/
/// `WORKS_ON` (an entity role's confidence is the implicit 100 the conflict
/// check reads it as — the view says so explicitly).
fn edge_from_row(raw: &EdgeRow) -> Result<GraphEdge, GraphError> {
    let id = raw.edge_id.to_string().trim_matches('"').to_string();
    let (kind, role, confidence_pct, source_kind) = match raw.edge_type.as_str() {
        "RELATIONSHIP" => (
            raw.kind.clone(),
            None,
            raw.confidence_pct.and_then(|p| i32::try_from(p).ok()),
            raw.source_kind.clone(),
        ),
        "HAS_ROLE" => (None, raw.role.clone(), Some(IMPLICIT_ROLE_CONFIDENCE), None),
        "WORKS_ON" => (None, raw.role.clone(), None, None),
        "MEMBER_OF" | "HANDLES" => (None, None, None, None),
        other => {
            tracing::warn!(edge_type = other, "unknown edge type in a graph view");
            return Err(GraphError::Traversal);
        }
    };
    Ok(GraphEdge {
        source: raw.from_id.clone(),
        target: raw.to_id.clone(),
        edge_type: raw.edge_type.clone(),
        id,
        kind,
        role,
        confidence_pct,
        source_kind,
    })
}

/// The confidence an entity role carries when no explicit number is stored —
/// the same implicit 100 `postgres_graph` documents.
const IMPLICIT_ROLE_CONFIDENCE: i32 = 100;

/// The bound payload for a frontier reader: one JSON object with the id array.
fn frontier_payload(frontier: &[String]) -> String {
    serde_json::json!({ "frontier": frontier }).to_string()
}

fn assemble_view(
    nodes: BTreeMap<String, GraphNode>,
    edges: BTreeMap<String, GraphEdge>,
) -> GraphView {
    GraphView {
        nodes: nodes.into_values().collect(),
        edges: edges.into_values().collect(),
    }
}

#[cfg(test)]
#[path = "postgres_graph_views_tests.rs"]
mod postgres_graph_views_tests;
