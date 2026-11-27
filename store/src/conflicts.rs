//! Pre-matter conflict check — the graph traversal that runs *before* a
//! Project is created.
//!
//! # The shape of the problem
//!
//! A law firm may not open a matter that is adverse to, or improperly
//! entangled with, a client it already serves (Model Rules 1.7 / 1.9,
//! imputed firm-wide by 1.10). Answering "would this new matter
//! conflict?" is a **graph reachability** question: start from the
//! proposed client and the proposed entity, walk the relationships, and
//! see whether you arrive at another party the firm already represents —
//! especially across an `adverse_to` edge.
//!
//! # Where the graph lives
//!
//! In PostgreSQL wherever it is configured (see
//! [`postgres_graph::graph_authority`]): the check walks the
//! `navigator_graph` projection in Apache AGE through
//! [`crate::postgres_graph`], hop by hop in openCypher (one fixed
//! single-type edge pattern per hop — see
//! `docs/age-cypher-parity-probe.md`), and reads the entity disclosures
//! from PostgreSQL. A deployment without PostgreSQL runs the original
//! SurrealQL walk over `<->entity_role` and
//! `<->relationship`. The endpoint names arrive inside the traversal rows
//! either way, so a finding explains itself identically whichever engine
//! served it.
//!
//! # This module only reads
//!
//! The traversal is `LET` and `SELECT` and nothing else. That mattered
//! less when the graph was a throwaway database — a stray write went
//! nowhere — and matters more now that the statements run against the
//! live store. [`tests::the_check_writes_nothing`] pins it.
//!
//! # What the engine does and what Rust does
//!
//! The engine walks: one bounded query collects every edge within
//! [`MAX_HOPS`] of the two anchors. Rust scores: the confidence-product
//! along a path, the floors, and the shortest-path-preferring revisit
//! rule stay here, because they decide *which* paths are worth
//! following and that is a judgment about conflicts, not about graphs.
//!
//! # Who may see what
//!
//! A conflict check reads **across matters, unscoped** — the parties it
//! looks for are by definition on someone else's matter, which is what
//! imputed firm-wide checking under Model Rule 1.10 means. The engine
//! does not narrow this and is not asked to: Navigator's authorization
//! lives above the database (#1145), and the containment is that only
//! firm-side create paths call in here. See
//! [`docs/access-model.md`](https://github.com/neon-law/navigator/blob/main/docs/access-model.md).
//!
//! # What feeds the graph
//!
//! - `entity_role` — structural ties (a person manages / owns / is a
//!   member of an entity). Always present, always confidence 100.
//! - `relationship` — the supplemental typed edges: adversity,
//!   related-party ties, and edges an LLM parsed out of a Relationship
//!   Log's detail. Each carries its own confidence and provenance.
//!
//! Findings are **advisory to clear, authoritative to block**: a
//! confident, direct `adverse_to` link to an existing client is a hard
//! block; everything else is surfaced for a human to adjudicate (the
//! firm's standing `@cleared` discipline). The graph can *raise* a
//! conflict; only a person can *clear* one — because the graph is never
//! known to be complete.

use std::collections::{HashMap, HashSet, VecDeque};

use uuid::Uuid;
use String;

use crate::application_store::ApplicationStore;
use crate::postgres::PostgresDb;
use crate::postgres_graph;
use crate::relationships::{Endpoint, KIND_ADVERSE_TO};

/// How many relationship hops out from the proposed matter the check
/// explores. Three reaches "my counterparty's affiliate's owner" — deep
/// enough for imputation without drowning lawyer in distant noise.
const MAX_HOPS: usize = 3;

/// A path weaker than this (after multiplying edge confidences) is
/// dropped — a chain of low-confidence guesses should not raise a
/// finding on its own.
const REVIEW_FLOOR_PCT: i32 = 25;

/// A `Block` requires at least this much confidence. Below it, even an
/// adverse link is downgraded to `Review` rather than hard-stopping the
/// open on a shaky edge.
const BLOCK_FLOOR_PCT: i32 = 80;

/// A `Block` requires the adverse counterparty to be this close — a
/// direct or one-removed adversity. Distant adversity is a review item.
const BLOCK_MAX_HOPS: usize = 2;

/// Whether PostgreSQL holds the graph is decided beside the graph
/// machinery: [`crate::postgres_graph`] owns the AGE walk and the
/// PostgreSQL write path.
use crate::postgres_graph::graph_authority;

/// A node in the conflict graph: a typed reference to one `person` or
/// `entity` row.
///
/// The kind is [`Endpoint`], which `store::relationships` owns because
/// the engine enforces it there — the relation's `FROM person|entity TO
/// person|entity` refuses anything else at write time, so this module
/// does not re-check an endpoint kind it reads back.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeRef {
    pub kind: Endpoint,
    pub id: Uuid,
}

impl NodeRef {
    /// A `person` node.
    #[must_use]
    pub fn person(id: Uuid) -> Self {
        Self {
            kind: Endpoint::Person,
            id,
        }
    }

    /// An `entity` node.
    #[must_use]
    pub fn entity(id: Uuid) -> Self {
        Self {
            kind: Endpoint::Entity,
            id,
        }
    }
}

/// How serious a finding is. `Block` hard-stops the automated open;
/// `Review` surfaces the finding but lets authorized lawyers proceed after
/// acknowledging it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Review,
    Block,
}

/// Why a finding fired — the legal shape of the concern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// The path crosses an `adverse_to` edge to a party the firm serves.
    Adverse,
    /// The proposed parties share an entity / party with another matter
    /// the firm already runs for a different client.
    SharedParty,
    /// A recorded `disclosures` row (conflict / related-party) touches a
    /// node in the proposed matter's neighborhood.
    Disclosure,
}

/// One conflict the check surfaced, with enough context for a lawyer to
/// adjudicate it rather than trust it blindly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictFinding {
    pub severity: Severity,
    pub reason: Reason,
    /// Human label of the party the proposed matter collides with.
    pub counterparty: String,
    /// Full sentence including the relationship path that produced it.
    pub explanation: String,
    /// Confidence the path is real, 0–100.
    pub confidence_pct: i32,
}

/// The result of a pre-matter conflict check.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConflictReport {
    pub findings: Vec<ConflictFinding>,
}

impl ConflictReport {
    /// No conflicts at all — the matter may open without lawyer review.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        self.findings.is_empty()
    }

    /// At least one `Block`-severity finding — the automated open is
    /// hard-stopped and cannot be overridden from the create form.
    #[must_use]
    pub fn has_blocking(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Block)
    }

    /// One human-readable line per finding, for the create form and for
    /// the Relationship Log audit entry when a lawyer overrides a review.
    #[must_use]
    pub fn summary_lines(&self) -> Vec<String> {
        self.findings
            .iter()
            .map(|f| {
                let tag = match f.severity {
                    Severity::Block => "BLOCK",
                    Severity::Review => "REVIEW",
                };
                format!(
                    "[{tag}] {} ({}% confidence)",
                    f.explanation, f.confidence_pct
                )
            })
            .collect()
    }
}

/// One edge the traversal returned, normalized across the two edge
/// tables: `entity_role`'s `role` and its implicit confidence 100 read
/// back in the same shape as `relationship`'s own fields.
///
/// The endpoint *names* travel with the edge. A link dereferences to a
/// real row, so asking for `in.name` in the same round trip is cheaper
/// than a separate load and keeps each name consistent with its edge.
struct TraversedEdge {
    edge_table: String,
    from_table: String,
    from_id: String,
    from_name: Option<String>,
    to_table: String,
    to_id: String,
    to_name: Option<String>,
    kind: String,
    confidence_pct: i32,
}

/// One step out of a node: the neighbor and the edge that reaches it.
struct Adjacent {
    node: NodeRef,
    kind: String,
    confidence_pct: i32,
}

/// What each reached node is called, for the sentence a finding prints.
type Labels = HashMap<NodeRef, String>;

/// The conflict graph's lookups, built once per check by [`build_graph`].
///
/// The graph itself is not in here — it is the store, and `graph` is a
/// handle to it. What this holds is the "who does the firm already
/// serve" side of the question, which is a matter of participations and
/// disclosures rather than of edges.
pub struct ConflictGraph {
    /// The Surreal store: every lookup here is a matter/participation/
    /// disclosure read, and the `surreal` engine's traversal runs on it
    /// too.
    graph: ApplicationStore,
    /// The PostgreSQL handle for AGE traversal.
    postgres: PostgresDb,
    /// Entity → the distinct client DRIs of its non-archived projects.
    entity_clients: HashMap<Uuid, HashSet<Uuid>>,
    /// Persons who are the client DRI of some non-archived project.
    client_persons: HashSet<Uuid>,
    /// Entity → its conflict / related-party disclosure summaries.
    entity_disclosures: HashMap<Uuid, Vec<String>>,
    /// Every non-archived project's own entity — [`ConflictGraph::findings_for_projects`]'s
    /// source for turning a caller's visible matters into graph anchors.
    project_entities: HashMap<Uuid, Uuid>,
}

/// Whether the firm has actually screened this person for conflicts.
///
/// The predicate is [`build_graph`]'s own definition of a person the firm
/// serves — a `person_project_role` carrying `is_client_dri` on a matter
/// that is not archived — asked about one person instead of all of them.
/// Two callers, one meaning: a party this returns `true` for is a party
/// the conflict graph already reasons over.
///
/// # Why not "does a `person` row exist"
///
/// Because a row is an identity, not a decision. Rows are minted by
/// `find_or_create` on the intake walk, by bulk import, by an OIDC first
/// sign-in and by seeding, none of which imply anyone ran a check. The
/// intake walk falsifies the shortcut outright: it writes the person
/// *before* it calls [`check_new_matter`], and when the check refuses the
/// intake the compensation removes the participation and the matter but
/// leaves the person — so mere presence is at its least trustworthy on
/// exactly the population the screen exists to catch.
///
/// The client-DRI marker does not have that problem. It is written by
/// `projects::designate_dri` only after the check has returned
/// without blocking, so it sits downstream of the screen by control flow
/// rather than beside it by coincidence.
///
/// Reads only, like the rest of this module.
///
/// # Errors
///
/// Returns a message if the participation or matter lookups fail.
pub async fn is_screened_client(
    application_store: &ApplicationStore,
    person_id: Uuid,
) -> Result<bool, String> {
    for role in crate::projects::participations_for_person(application_store, person_id)
        .await
        .map_err(|error| format!("load conflict-screen participations: {error}"))?
        .into_iter()
        .filter(|role| role.is_client_dri)
    {
        let matter = crate::projects::find_by_id(application_store, role.project_id)
            .await
            .map_err(|error| format!("load conflict-screen matter: {error}"))?;
        if matter.is_some_and(|matter| matter.status != "archived") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Load the lookups a conflict check tests its reachable set against.
///
/// The edges are not loaded here: the traversal reads them itself, one
/// bounded query at check time, rather than pulling them into memory
/// first. What this loads is `projects` (who the firm currently acts for)
/// and `disclosures`, the latter from the store that holds the graph.
///
/// # Errors
///
/// Returns a [`String::Custom`] if any of the matter, participation, or
/// disclosure lookups fail, or PostgreSQL is configured but unreachable.
pub async fn build_graph(application_store: &ApplicationStore) -> Result<ConflictGraph, String> {
    let postgres = graph_authority().await?;
    // Who the firm currently acts for, keyed by entity. An under-inclusive
    // set here is a missed conflict, so this stays one bulk join over the
    // client-DRI markers rather than a per-project lookup.
    let mut entity_clients: HashMap<Uuid, HashSet<Uuid>> = HashMap::new();
    let mut client_persons: HashSet<Uuid> = HashSet::new();
    let live_project_entities: HashMap<Uuid, Uuid> = crate::projects::all(application_store)
        .await
        .map_err(|error| format!("load conflict-check matters: {error}"))?
        .into_iter()
        .filter(|project| project.status != "archived")
        .map(|proj| (proj.id, proj.entity_id))
        .collect();
    for row in crate::projects::all_participations(application_store)
        .await
        .map_err(|error| format!("load conflict-check participations: {error}"))?
        .into_iter()
        .filter(|role| role.is_client_dri)
    {
        let Some(entity_id) = live_project_entities.get(&row.project_id) else {
            continue;
        };
        entity_clients
            .entry(*entity_id)
            .or_default()
            .insert(row.person_id);
        client_persons.insert(row.person_id);
    }

    let entity_disclosures = crate::disclosures::conflict_summaries_by_entity(&postgres)
        .await
        .map_err(|error| format!("load conflict-check disclosures: {error}"))?;

    Ok(ConflictGraph {
        graph: application_store.clone(),
        postgres,
        entity_clients,
        client_persons,
        entity_disclosures,
        project_entities: live_project_entities,
    })
}

/// How a graph-engine failure reaches a caller that only knows
/// [`String`]. The store's public seam predates the second engine and
/// keeps its signature, so this is the one place the two error
/// vocabularies meet.
fn graph_engine_error(context: &str, source: &dyn std::fmt::Display) -> String {
    format!("{context}: {source}")
}

/// Read one traversal endpoint back into a [`NodeRef`].
///
/// Returns `None` for a table this check does not model or a key that
/// is not a UUID — "skip it rather than guess". The engine refuses to
/// *write* an endpoint outside `person|entity`, so this is a read-side
/// backstop rather than a live filter.
fn node_ref(table: &str, id: &str) -> Option<NodeRef> {
    Some(NodeRef {
        kind: Endpoint::from_table(table)?,
        id: Uuid::parse_str(id).ok()?,
    })
}

/// What a node is called when its row is missing.
fn fallback_label(node: NodeRef) -> String {
    match node.kind {
        Endpoint::Person => format!("person {}", node.id),
        Endpoint::Entity => format!("entity {}", node.id),
    }
}

/// The label a node prints in a finding.
fn label(labels: &Labels, node: NodeRef) -> String {
    labels
        .get(&node)
        .cloned()
        .unwrap_or_else(|| fallback_label(node))
}

/// One node the traversal reached, with how it got there.
struct Reached {
    confidence_pct: i32,
    hops: usize,
    adverse_on_path: bool,
    path: String,
}

/// An AGE traversal row read back in the traversal's own shape. The
/// names travel with the edge in both engines' rows, so the adjacency
/// build below is engine-blind.
impl From<postgres_graph::TraversalEdge> for TraversedEdge {
    fn from(row: postgres_graph::TraversalEdge) -> Self {
        Self {
            edge_table: row.edge_table,
            from_table: row.from_table,
            from_id: row.from_id,
            from_name: row.from_name,
            to_table: row.to_table,
            to_id: row.to_id,
            to_name: row.to_name,
            kind: row.kind,
            confidence_pct: row.confidence_pct,
        }
    }
}

impl ConflictGraph {
    /// Ask the engine for every edge within [`MAX_HOPS`] of `anchors`,
    /// as an undirected step list per node, plus what each reached node
    /// is called.
    ///
    /// The list is ordered — structural ties before supplemental ones,
    /// then by endpoint — so two runs over the same rows produce the
    /// same explanation text, which an engine's edge order does not
    /// guarantee on its own.
    async fn neighborhood(
        &self,
        anchors: &[NodeRef],
    ) -> Result<(HashMap<NodeRef, Vec<Adjacent>>, Labels), String> {
        let mut edges: Vec<TraversedEdge> =
            postgres_graph::conflict_traversal(&self.postgres, anchors)
                .await
                .map_err(|err| graph_engine_error("traverse the conflict-check graph", &err))?
                .into_iter()
                .map(TraversedEdge::from)
                .collect();

        edges.sort_by(|a, b| {
            (
                &a.edge_table,
                &a.from_table,
                &a.from_id,
                &a.to_table,
                &a.to_id,
                &a.kind,
            )
                .cmp(&(
                    &b.edge_table,
                    &b.from_table,
                    &b.from_id,
                    &b.to_table,
                    &b.to_id,
                    &b.kind,
                ))
        });

        let mut adjacency: HashMap<NodeRef, Vec<Adjacent>> = HashMap::new();
        let mut labels: Labels = HashMap::new();
        for edge in edges {
            let (Some(from), Some(to)) = (
                node_ref(&edge.from_table, &edge.from_id),
                node_ref(&edge.to_table, &edge.to_id),
            ) else {
                continue;
            };
            // A node whose row is gone still renders: the label falls
            // back to the id rather than the finding failing to print.
            if let Some(name) = edge.from_name {
                labels.insert(from, name);
            }
            if let Some(name) = edge.to_name {
                labels.insert(to, name);
            }
            // The graph is undirected: adversity runs both ways, and a
            // conflict does not care which end of the edge was typed
            // in first.
            adjacency.entry(from).or_default().push(Adjacent {
                node: to,
                kind: edge.kind.clone(),
                confidence_pct: edge.confidence_pct,
            });
            adjacency.entry(to).or_default().push(Adjacent {
                node: from,
                kind: edge.kind,
                confidence_pct: edge.confidence_pct,
            });
        }
        Ok((adjacency, labels))
    }

    /// Name the anchors themselves.
    ///
    /// The traversal names every node it *returns*, but an anchor with
    /// no edges at all appears in no edge — and it is still the subject
    /// of the report, so it has to be resolvable. Only the anchors
    /// missing from the traversal are read back.
    async fn label_anchors(&self, anchors: &[NodeRef], labels: &mut Labels) -> Result<(), String> {
        let missing: Vec<NodeRef> = anchors
            .iter()
            .copied()
            .filter(|anchor| !labels.contains_key(anchor))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }

        let person_ids: Vec<Uuid> = missing
            .iter()
            .filter(|n| n.kind == Endpoint::Person)
            .map(|n| n.id)
            .collect();
        let entity_ids: Vec<Uuid> = missing
            .iter()
            .filter(|n| n.kind == Endpoint::Entity)
            .map(|n| n.id)
            .collect();

        for person in crate::persons::find_by_ids(&self.graph, &person_ids)
            .await
            .map_err(|err| graph_engine_error("resolve a conflict-check anchor person", &err))?
        {
            labels.insert(NodeRef::person(person.id), person.name);
        }
        for entity in crate::entities::find_by_ids(&self.graph, &entity_ids)
            .await
            .map_err(|err| graph_engine_error("resolve a conflict-check anchor entity", &err))?
        {
            labels.insert(NodeRef::entity(entity.id), entity.name);
        }
        Ok(())
    }

    /// Breadth-first walk from both anchors, keeping the shortest path to
    /// each reachable node along with the multiplied confidence and
    /// whether an `adverse_to` edge lay on the way.
    async fn reach(
        &self,
        anchors: &[NodeRef],
    ) -> Result<(HashMap<NodeRef, Reached>, Labels), String> {
        let (adjacency, mut labels) = self.neighborhood(anchors).await?;
        self.label_anchors(anchors, &mut labels).await?;

        let mut reached: HashMap<NodeRef, Reached> = HashMap::new();
        let mut queue: VecDeque<(NodeRef, i32, usize, bool, String)> = VecDeque::new();

        for &a in anchors {
            reached.insert(
                a,
                Reached {
                    confidence_pct: 100,
                    hops: 0,
                    adverse_on_path: false,
                    path: label(&labels, a),
                },
            );
            queue.push_back((a, 100, 0, false, label(&labels, a)));
        }

        while let Some((cur, conf, hops, adverse, path)) = queue.pop_front() {
            if hops >= MAX_HOPS {
                continue;
            }
            let Some(neighbors) = adjacency.get(&cur) else {
                continue;
            };
            for step in neighbors {
                let next = step.node;
                let next_conf = conf * step.confidence_pct / 100;
                if next_conf < REVIEW_FLOOR_PCT {
                    continue;
                }
                let next_adverse = adverse || step.kind == KIND_ADVERSE_TO;
                let next_path = format!("{path} —{}→ {}", step.kind, label(&labels, next));
                let next_hops = hops + 1;
                let should_update = reached.get(&next).is_none_or(|existing| {
                    next_hops < existing.hops
                        || (next_adverse && !existing.adverse_on_path)
                        || (next_conf > existing.confidence_pct
                            && (next_adverse || !existing.adverse_on_path))
                });
                if should_update {
                    reached.insert(
                        next,
                        Reached {
                            confidence_pct: next_conf,
                            hops: next_hops,
                            adverse_on_path: next_adverse,
                            path: next_path.clone(),
                        },
                    );
                    queue.push_back((next, next_conf, next_hops, next_adverse, next_path));
                }
            }
        }
        Ok((reached, labels))
    }

    /// Run the conflict check for opening a matter for `client_person_id`
    /// against `entity_id`. The anchors are those two nodes; the report
    /// names every distinct firm-served party the proposed matter is
    /// entangled with.
    ///
    /// # Errors
    ///
    /// Returns a [`String::Custom`] if the graph engine cannot answer the
    /// traversal.
    pub async fn check(
        &self,
        client_person_id: Uuid,
        entity_id: Uuid,
    ) -> Result<ConflictReport, String> {
        let anchor_person = NodeRef::person(client_person_id);
        let anchor_entity = NodeRef::entity(entity_id);
        let (reached, labels) = self.reach(&[anchor_person, anchor_entity]).await?;

        let mut findings = Vec::new();
        for (&n, r) in &reached {
            // Adversity / shared-party concerns attach to entity nodes the
            // firm already serves and to *other* client persons.
            let counterparty_client = match n.kind {
                Endpoint::Entity => self
                    .entity_clients
                    .get(&n.id)
                    .is_some_and(|clients| clients.iter().any(|c| *c != client_person_id)),
                Endpoint::Person => n.id != client_person_id && self.client_persons.contains(&n.id),
            };

            if counterparty_client {
                let (severity, reason) = if r.adverse_on_path {
                    let blocking = r.confidence_pct >= BLOCK_FLOOR_PCT && r.hops <= BLOCK_MAX_HOPS;
                    (
                        if blocking {
                            Severity::Block
                        } else {
                            Severity::Review
                        },
                        Reason::Adverse,
                    )
                } else {
                    (Severity::Review, Reason::SharedParty)
                };
                let lead = match reason {
                    Reason::Adverse => "Adverse to a current client",
                    _ => "Shares a party with a current client's matter",
                };
                findings.push(ConflictFinding {
                    severity,
                    reason,
                    counterparty: label(&labels, n),
                    explanation: format!("{lead}: {}", r.path),
                    confidence_pct: r.confidence_pct,
                });
            }

            // Recorded disclosures on any reached entity always surface.
            if n.kind == Endpoint::Entity {
                if let Some(summaries) = self.entity_disclosures.get(&n.id) {
                    for summary in summaries {
                        findings.push(ConflictFinding {
                            severity: Severity::Review,
                            reason: Reason::Disclosure,
                            counterparty: label(&labels, n),
                            explanation: format!(
                                "Disclosure on {}: {summary} (via {})",
                                label(&labels, n),
                                r.path
                            ),
                            confidence_pct: r.confidence_pct,
                        });
                    }
                }
            }
        }

        // Stable order: blocks first, then by descending confidence, so
        // the most serious finding leads the form and the audit log.
        findings.sort_by(|a, b| {
            b.severity
                .eq(&Severity::Block)
                .cmp(&a.severity.eq(&Severity::Block))
                .then(b.confidence_pct.cmp(&a.confidence_pct))
                .then(a.counterparty.cmp(&b.counterparty))
        });
        Ok(ConflictReport { findings })
    }

    /// Ranked conflict findings across the firm-wide graph, anchored on the
    /// given projects' own entities and current client DRIs (ENG-307) —
    /// the lawyer dashboard's reading of "their" conflicts, per
    /// `docs/access-model.md`'s Model Rule 1.10 discussion: firm-wide,
    /// oriented on the caller's own matters rather than scoped to them, so
    /// a conflict arising out of a matter the caller does not participate
    /// in still surfaces.
    ///
    /// Distinct from [`check`](Self::check), which answers "would *this*
    /// proposed new matter conflict" for a client/entity pair that may not
    /// have a project yet. This answers "what does the firm's graph say
    /// about the matters I already have", anchored on projects that already
    /// exist.
    ///
    /// A reached node is a finding only when it is a client the firm serves
    /// on some *other* project — one of the anchor projects' own entity or
    /// client DRI is the matter itself, not a conflict with it.
    ///
    /// # Errors
    ///
    /// Returns a [`String`] if the participation lookup or the traversal
    /// fails.
    pub async fn findings_for_projects(
        &self,
        project_ids: &[Uuid],
    ) -> Result<Vec<ConflictFinding>, String> {
        let anchor_projects: HashSet<Uuid> = project_ids.iter().copied().collect();
        if anchor_projects.is_empty() {
            return Ok(Vec::new());
        }

        let anchor_entities: HashSet<Uuid> = anchor_projects
            .iter()
            .filter_map(|id| self.project_entities.get(id))
            .copied()
            .collect();
        let anchor_clients: HashSet<Uuid> = crate::projects::all_participations(&self.graph)
            .await
            .map_err(|error| format!("load conflict-dashboard participations: {error}"))?
            .into_iter()
            .filter(|role| role.is_client_dri && anchor_projects.contains(&role.project_id))
            .map(|role| role.person_id)
            .collect();

        let anchors: Vec<NodeRef> = anchor_entities
            .iter()
            .copied()
            .map(NodeRef::entity)
            .chain(anchor_clients.iter().copied().map(NodeRef::person))
            .collect();
        if anchors.is_empty() {
            return Ok(Vec::new());
        }

        let (reached, labels) = self.reach(&anchors).await?;

        let mut findings = Vec::new();
        for (&n, r) in &reached {
            // The anchors themselves are the caller's own matters, not a
            // conflict with them.
            if r.hops == 0 {
                continue;
            }
            let counterparty_client = match n.kind {
                Endpoint::Entity => {
                    !anchor_entities.contains(&n.id)
                        && self.entity_clients.get(&n.id).is_some_and(|clients| {
                            clients.iter().any(|c| !anchor_clients.contains(c))
                        })
                }
                Endpoint::Person => {
                    !anchor_clients.contains(&n.id) && self.client_persons.contains(&n.id)
                }
            };

            if counterparty_client {
                let (severity, reason) = if r.adverse_on_path {
                    let blocking = r.confidence_pct >= BLOCK_FLOOR_PCT && r.hops <= BLOCK_MAX_HOPS;
                    (
                        if blocking {
                            Severity::Block
                        } else {
                            Severity::Review
                        },
                        Reason::Adverse,
                    )
                } else {
                    (Severity::Review, Reason::SharedParty)
                };
                let lead = match reason {
                    Reason::Adverse => "Adverse to a current client",
                    _ => "Shares a party with a current client's matter",
                };
                findings.push(ConflictFinding {
                    severity,
                    reason,
                    counterparty: label(&labels, n),
                    explanation: format!("{lead}: {}", r.path),
                    confidence_pct: r.confidence_pct,
                });
            }

            if n.kind == Endpoint::Entity {
                if let Some(summaries) = self.entity_disclosures.get(&n.id) {
                    for summary in summaries {
                        findings.push(ConflictFinding {
                            severity: Severity::Review,
                            reason: Reason::Disclosure,
                            counterparty: label(&labels, n),
                            explanation: format!(
                                "Disclosure on {}: {summary} (via {})",
                                label(&labels, n),
                                r.path
                            ),
                            confidence_pct: r.confidence_pct,
                        });
                    }
                }
            }
        }

        findings.sort_by(|a, b| {
            b.severity
                .eq(&Severity::Block)
                .cmp(&a.severity.eq(&Severity::Block))
                .then(b.confidence_pct.cmp(&a.confidence_pct))
                .then(a.counterparty.cmp(&b.counterparty))
        });
        Ok(findings)
    }
}

/// Build the lookups and run the pre-matter conflict check in one call.
/// This is the entry point the project-create paths use.
///
/// # Errors
///
/// Returns any `String` from loading the lookups or running the
/// traversal.
pub async fn check_new_matter(
    application_store: &ApplicationStore,
    client_person_id: Uuid,
    entity_id: Uuid,
) -> Result<ConflictReport, String> {
    build_graph(application_store)
        .await?
        .check(client_person_id, entity_id)
        .await
}

/// Build the lookups and run the lawyer-dashboard conflict scan in one
/// call (ENG-307) — see [`ConflictGraph::findings_for_projects`]. This is
/// the entry point `webapp::lawyer_dashboard` uses; it answers a different
/// question from [`check_new_matter`] and does not replace it.
///
/// # Errors
///
/// Returns any `String` from loading the lookups or running the
/// traversal.
pub async fn findings_for_matters(
    application_store: &ApplicationStore,
    project_ids: &[Uuid],
) -> Result<Vec<ConflictFinding>, String> {
    build_graph(application_store)
        .await?
        .findings_for_projects(project_ids)
        .await
}

/// One edge of the conflict-check graph between two given nodes: a
/// structural `entity_role` or a typed `relationship`, read back in one
/// shape. `kind` is the role (`manages`, `owns`, …) or the relationship
/// kind (`adverse_to`, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: NodeRef,
    pub to: NodeRef,
    pub kind: String,
    pub confidence_pct: i32,
}

/// Every graph edge whose two ends are both in `nodes`, structural ties
/// first, in a stable order.
///
/// The read a matter page's relationship graph draws: it never leaves the
/// node set it is given, so a page built from one matter's parties shows
/// only the ties among those parties, never a hop into another matter.
/// Reads only, like the rest of this module.
///
/// # Errors
///
/// Returns a message if the query fails.
pub async fn edges_among(
    _application_store: &ApplicationStore,
    nodes: &[NodeRef],
) -> Result<Vec<Edge>, String> {
    if nodes.is_empty() {
        return Ok(Vec::new());
    }
    let postgres = graph_authority().await?;
    let rows = postgres_graph::conflict_frontier_edges(&postgres, nodes)
        .await
        .map_err(|err| graph_engine_error("read the relationship graph", &err))?
        .into_iter()
        .map(TraversedEdge::from)
        .collect();
    Ok(edges_among_rows(rows, nodes))
}

/// Restrict a traversal's edges to those whose both endpoints sit in
/// `nodes`, in the stable order the matter-page graph draws — the one
/// tail both engines share.
fn edges_among_rows(rows: Vec<TraversedEdge>, nodes: &[NodeRef]) -> Vec<Edge> {
    let mut rows = rows;
    rows.sort_by(|a, b| {
        (&a.edge_table, &a.from_id, &a.to_id, &a.kind).cmp(&(
            &b.edge_table,
            &b.from_id,
            &b.to_id,
            &b.kind,
        ))
    });
    let wanted: HashSet<NodeRef> = nodes.iter().copied().collect();
    rows.into_iter()
        .filter_map(|row| {
            let from = node_ref(&row.from_table, &row.from_id)?;
            let to = node_ref(&row.to_table, &row.to_id)?;
            (wanted.contains(&from) && wanted.contains(&to)).then_some(Edge {
                from,
                to,
                kind: row.kind,
                confidence_pct: row.confidence_pct,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {}
