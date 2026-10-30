//! The graph views against a real AGE 1.6.0 database. The fixture is
//! richer than the parity fixture: two firms, two matters, and every
//! person-level edge family, so each view's expected neighborhood and the
//! firm aggregation's four tie kinds are all exercised. Views read only the
//! PostgreSQL mirrors and the projection — no Surreal side is seeded, since
//! these surfaces have no Surreal counterpart to stay in parity with.

use super::{firm_view, lawyer_view, project_view, FirmView, GraphView, MAX_GRAPH_DEPTH};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::postgres::PostgresDb;
use std::collections::BTreeSet;

/// A synthetic two-firm graph: Firm A holds two lawyers (lead and
/// co-counsel), Firm B a client and an opposing lawyer. Matter One carries
/// the firm-vs-client participation; Matter Two is shared firm-side work.
/// The projection must be rebuilt after seeding (`project_graph`).
async fn seed(pg: &PostgresDb) -> Fixture {
    let inserted_at = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);

    let (entity, lead, counsel, client, opposing) = seed_reference(pg, inserted_at).await;
    let fixture = seed_edges(pg, inserted_at, entity, lead, counsel, client, opposing).await;
    crate::postgres_graph::project_graph(pg).await.unwrap();
    fixture
}

/// The reference rows and the four people, in both stores' shape: the
/// entity types and jurisdiction `seed_entity` names, the entity, and the
/// lead lawyer, co-counsel, client person, and opposing lawyer.
async fn seed_reference(
    pg: &PostgresDb,
    inserted_at: chrono::DateTime<chrono::Utc>,
) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid, uuid::Uuid, uuid::Uuid) {

    sqlx::query(
        "INSERT INTO entity_type (id, name, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Entity Type', $2, $2)",
    )
    .bind(crate::test_support::SEED_ENTITY_TYPE_ID)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jurisdiction (id, name, code, jurisdiction_type, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Jurisdiction', 'ZZ', 'state', $2, $2)",
    )
    .bind(crate::test_support::SEED_ENTITY_JURISDICTION_ID)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();

    let entity = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO entity (id, name, entity_type_id, jurisdiction_id, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Entity', $2, $3, $4, $4)",
    )
    .bind(entity)
    .bind(crate::test_support::SEED_ENTITY_TYPE_ID)
    .bind(crate::test_support::SEED_ENTITY_JURISDICTION_ID)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();

    // Lead lawyer, co-counsel, client, opposing lawyer — in that order.
    let mut people = Vec::new();
    for name in ["Lead Lawyer", "Co-counsel", "Client Person", "Opposing Lawyer"] {
        let id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO person (id, name, email, inserted_at, updated_at) \
             VALUES ($1, $2, $3, $4, $4)",
        )
        .bind(id)
        .bind(name)
        .bind(format!("{id}@example.com"))
        .bind(inserted_at)
        .execute(&pg.0)
        .await
        .unwrap();
        people.push(id);
    }
    (entity, people[0], people[1], people[2], people[3])
}

/// The matters, firms, and every edge-family row the graph projection
/// carries: the entity role, both relationships, the participations, and
/// the firm memberships.
async fn seed_edges(
    pg: &PostgresDb,
    inserted_at: chrono::DateTime<chrono::Utc>,
    entity: uuid::Uuid,
    lead: uuid::Uuid,
    counsel: uuid::Uuid,
    client: uuid::Uuid,
    opposing: uuid::Uuid,
) -> Fixture {
    let matter_one = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO project (id, code, name, status, entity_id, inserted_at, updated_at) \
         VALUES ($1, 'SYN1', 'Matter One', 'active', $2, $3, $3)",
    )
    .bind(matter_one)
    .bind(entity)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    let matter_two = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO project (id, code, name, status, entity_id, inserted_at, updated_at) \
         VALUES ($1, 'SYN2', 'Matter Two', 'active', $2, $3, $3)",
    )
    .bind(matter_two)
    .bind(entity)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();

    let firm_a = uuid::Uuid::now_v7();
    let firm_b = uuid::Uuid::now_v7();
    for (id, name, slug) in [
        (firm_a, "Firm A", "firm-a"),
        (firm_b, "Firm B", "firm-b"),
    ] {
        sqlx::query(
            "INSERT INTO firm (id, name, slug, status, inserted_at, updated_at) \
             VALUES ($1, $2, $3, 'active', $4, $4)",
        )
        .bind(id)
        .bind(name)
        .bind(slug)
        .bind(inserted_at)
        .execute(&pg.0)
        .await
        .unwrap();
    }

    seed_role_and_relationships(pg, inserted_at, entity, lead, client, opposing).await;

    // Matter One: lead lawyer firm-side, client person client-side.
    // Matter Two: co-counsel (Firm A) and opposing lawyer (Firm B), both
    // firm-side — the co-counsel tie.
    for (person, project, participation) in [
        (lead, matter_one, "lead"),
        (client, matter_one, "client"),
        (counsel, matter_two, "lead"),
        (opposing, matter_two, "lead"),
    ] {
        sqlx::query(
            "INSERT INTO person_project_role (person_id, project_id, participation, \
             inserted_at, updated_at) VALUES ($1, $2, $3, $4, $4)",
        )
        .bind(person)
        .bind(project)
        .bind(participation)
        .bind(inserted_at)
        .execute(&pg.0)
        .await
        .unwrap();
    }

    // Firm A staffs both lawyers; Firm B the client and the opposing
    // lawyer. (Membership values are constrained to admin/lawyer/clerk and
    // do not reach the graph — only the MEMBER_OF edge does.)
    for (person, firm, membership) in [
        (lead, firm_a, "lawyer"),
        (counsel, firm_a, "lawyer"),
        (client, firm_b, "clerk"),
        (opposing, firm_b, "lawyer"),
    ] {
        sqlx::query(
            "INSERT INTO person_firm_role (person_id, firm_id, membership, inserted_at, \
             updated_at) VALUES ($1, $2, $3, $4, $4)",
        )
        .bind(person)
        .bind(firm)
        .bind(membership)
        .bind(inserted_at)
        .execute(&pg.0)
        .await
        .unwrap();
    }

    Fixture {
        lead,
        counsel,
        client,
        opposing,
        matter_one,
        matter_two,
        entity,
        firm_a,
        firm_b,
    }
}

/// The lead lawyer manages the entity, is adverse to the client, and is
/// related to the opposing lawyer — the person-level edge rows the firm
/// view's adversary and of-counsel arms aggregate from.
async fn seed_role_and_relationships(
    pg: &PostgresDb,
    inserted_at: chrono::DateTime<chrono::Utc>,
    entity: uuid::Uuid,
    lead: uuid::Uuid,
    client: uuid::Uuid,
    opposing: uuid::Uuid,
) {
    sqlx::query(
        "INSERT INTO entity_role (person_id, entity_id, role, inserted_at, updated_at) \
         VALUES ($1, $2, 'manages', $3, $3)",
    )
    .bind(lead)
    .bind(entity)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    for (from, to, kind, confidence, source) in [
        (lead, client, "adverse_to", 80, "manual"),
        (lead, opposing, "related_party", 60, "disclosure"),
    ] {
        sqlx::query(
            "INSERT INTO \"relationship\" (id, \"in\", \"out\", kind, confidence_pct, \
             source_kind, inserted_at, updated_at) \
             VALUES ($1, $2::jsonb, $3::jsonb, $4, $5, $6, $7, $7)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(serde_json::json!({"tb": "person", "id": {"Uuid": from.to_string()}}).to_string())
        .bind(serde_json::json!({"tb": "person", "id": {"Uuid": to.to_string()}}).to_string())
        .bind(kind)
        .bind(confidence)
        .bind(source)
        .bind(inserted_at)
        .execute(&pg.0)
        .await
        .unwrap();
    }
}

struct Fixture {
    lead: uuid::Uuid,
    counsel: uuid::Uuid,
    client: uuid::Uuid,
    opposing: uuid::Uuid,
    matter_one: uuid::Uuid,
    matter_two: uuid::Uuid,
    entity: uuid::Uuid,
    firm_a: uuid::Uuid,
    firm_b: uuid::Uuid,
}

/// The rendered edge keys of a view: `type source->target` (with the
/// relationship kind where one exists), sorted — assertion-friendly and
/// direction-canonical (the endpoints are the stored edge's start/end).
fn edge_keys(view: &GraphView) -> Vec<String> {
    view.edges
        .iter()
        .map(|edge| match &edge.kind {
            Some(kind) => format!("{} {}->{} ({kind})", edge.edge_type, edge.source, edge.target),
            None => format!("{} {}->{}", edge.edge_type, edge.source, edge.target),
        })
        .collect()
}

fn node_ids(view: &GraphView) -> BTreeSet<String> {
    view.nodes.iter().map(|node| node.id.clone()).collect()
}

#[tokio::test]
async fn project_view_renders_the_matter_neighborhood() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let f = seed(&pg).await;

    // Depth 2: the matter, its parties, the handling firms, and one
    // relationship hop outward (the opposing lawyer, related to the lead).
    let view = project_view(&pg, f.matter_one, 2).await.unwrap();
    let ids = node_ids(&view);
    for id in [
        f.matter_one, f.lead, f.client, f.opposing, f.entity, f.firm_a, f.firm_b,
    ] {
        assert!(ids.contains(&id.to_string()), "missing {id} in {ids:?}");
    }
    assert!(!ids.contains(&f.counsel.to_string()), "co-counsel works Matter Two only");
    assert!(!ids.contains(&f.matter_two.to_string()), "Matter Two is not on Matter One's view");
    assert_eq!(view.nodes.len(), 7);

    let keys = edge_keys(&view);
    assert!(keys.contains(&format!("WORKS_ON {}->{}", f.lead, f.matter_one)));
    assert!(keys.contains(&format!("WORKS_ON {}->{}", f.client, f.matter_one)));
    assert!(keys.contains(&format!("HANDLES {}->{}", f.firm_a, f.matter_one)));
    assert!(keys.contains(&format!("HANDLES {}->{}", f.firm_b, f.matter_one)));
    assert!(keys.contains(&format!(
        "RELATIONSHIP {}->{} (adverse_to)",
        f.lead, f.client
    )));
    assert!(keys.contains(&format!(
        "RELATIONSHIP {}->{} (related_party)",
        f.lead, f.opposing
    )));
    assert!(keys.contains(&format!("HAS_ROLE {}->{}", f.lead, f.entity)));
    assert!(keys.contains(&format!("MEMBER_OF {}->{}", f.lead, f.firm_a)));
    assert!(keys.contains(&format!("MEMBER_OF {}->{}", f.client, f.firm_b)));
    assert_eq!(
        view.edges.len(),
        9,
        "the opposing lawyer's own membership and matter are one hop further"
    );

    // The adverse edge carries its provenance.
    let adverse = view
        .edges
        .iter()
        .find(|edge| edge.edge_type == "RELATIONSHIP" && edge.kind.as_deref() == Some("adverse_to"))
        .unwrap();
    assert_eq!(adverse.confidence_pct, Some(80));
    assert_eq!(adverse.source_kind.as_deref(), Some("manual"));

    // The entity role reads as the implicit 100 the conflict check uses.
    let role = view
        .edges
        .iter()
        .find(|edge| edge.edge_type == "HAS_ROLE")
        .unwrap();
    assert_eq!(role.role.as_deref(), Some("manages"));
    assert_eq!(role.confidence_pct, Some(100));

    // Depth 1 stops at the matter's own attachments.
    let shallow = project_view(&pg, f.matter_one, 1).await.unwrap();
    assert_eq!(shallow.nodes.len(), 5, "matter, two parties, two firms");
    assert_eq!(shallow.edges.len(), 4, "two works_on, two handles");

    // Depth beyond the server cap clamps to the cap — same as depth 3.
    let deep = project_view(&pg, f.matter_one, MAX_GRAPH_DEPTH).await.unwrap();
    let clamped = project_view(&pg, f.matter_one, 255).await.unwrap();
    assert_eq!(deep, clamped);

    // Depth 3 expands the parties the second hop newly reached: the
    // opposing lawyer's firm membership comes in. Their other matter does
    // not — the project expand reader returns no WORKS_ON rows, so a
    // party's unrelated matters never balloon the matter's view.
    let deep_keys = edge_keys(&deep);
    assert!(deep_keys.contains(&format!("MEMBER_OF {}->{}", f.opposing, f.firm_b)));
    assert!(!deep_keys.iter().any(|key| key.contains("WORKS_ON") && key.contains(&f.matter_two.to_string())));
    assert_eq!(deep.nodes.len(), 7);
    assert_eq!(deep.edges.len(), 10);
}

#[tokio::test]
async fn lawyer_view_renders_the_person_neighborhood() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let f = seed(&pg).await;

    let view = lawyer_view(&pg, f.lead, 2).await.unwrap();
    let ids = node_ids(&view);
    for id in [
        f.lead, f.matter_one, f.entity, f.firm_a, f.client, f.opposing, f.firm_b, f.matter_two,
    ] {
        assert!(
            ids.contains(&id.to_string()),
            "missing {id} in {ids:?}"
        );
    }
    assert_eq!(
        view.nodes.len(),
        8,
        "two expansion rounds reach the opposing lawyer's own matter"
    );

    let keys = edge_keys(&view);
    assert!(keys.contains(&format!("WORKS_ON {}->{}", f.lead, f.matter_one)));
    assert!(keys.contains(&format!("WORKS_ON {}->{}", f.client, f.matter_one)));
    assert!(keys.contains(&format!("HAS_ROLE {}->{}", f.lead, f.entity)));
    assert!(keys.contains(&format!("MEMBER_OF {}->{}", f.lead, f.firm_a)));
    assert!(keys.contains(&format!("MEMBER_OF {}->{}", f.client, f.firm_b)));
    assert!(
        view.nodes
            .iter()
            .any(|node| node.id == f.client.to_string()
                && node.name.as_deref() == Some("Client Person")),
        "expansion-discovered nodes carry the vertex name property"
    );

    assert!(keys.contains(&format!("MEMBER_OF {}->{}", f.opposing, f.firm_b)));
    assert!(keys.contains(&format!("WORKS_ON {}->{}", f.opposing, f.matter_two)));
    assert!(keys.contains(&format!(
        "RELATIONSHIP {}->{} (adverse_to)",
        f.lead, f.client
    )));
    assert!(keys.contains(&format!(
        "RELATIONSHIP {}->{} (related_party)",
        f.lead, f.opposing
    )));
    assert_eq!(view.edges.len(), 9);
}

#[tokio::test]
async fn firm_view_aggregates_person_edges_into_firm_ties() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let f = seed(&pg).await;

    let FirmView { nodes, edges } = firm_view(&pg, f.firm_a).await.unwrap();

    let ids: BTreeSet<String> = nodes.iter().map(|node| node.id.clone()).collect();
    let expected: BTreeSet<String> = [f.firm_a, f.firm_b]
        .map(|id| id.to_string())
        .into_iter()
        .collect();
    assert_eq!(ids, expected, "the seed firm and its one counterpart");

    // Every tie kind fires exactly once on this fixture, Firm A -> Firm B.
    let mut by_kind = std::collections::BTreeMap::new();
    for edge in &edges {
        assert_eq!(edge.source, f.firm_a.to_string());
        assert_eq!(edge.target, f.firm_b.to_string());
        assert_eq!(edge.edge_type, "FIRM_TIE");
        by_kind.insert(edge.kind.as_str(), edge.strength);
    }
    assert_eq!(
        by_kind,
        [
            ("adversary", 1),
            ("client", 1),
            ("co_counsel", 1),
            ("of_counsel", 1)
        ]
        .into_iter()
        .map(|(kind, strength)| (kind, i64::from(strength)))
        .collect(),
        "adverse_to (lead vs client), client (client person on Matter One), \
         co_counsel (shared firm-side Matter Two), of_counsel (related_party)"
    );

    // The view is depth-independent: the aggregation is one read regardless
    // of how the caller caps it.
    let again = firm_view(&pg, f.firm_a).await.unwrap();
    assert_eq!(again.edges, edges);
}

#[tokio::test]
async fn views_render_an_empty_neighborhood_for_an_unprojected_seed() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let missing = uuid::Uuid::now_v7();

    let project = project_view(&pg, missing, 2).await.unwrap();
    assert_eq!(project.nodes.len(), 0);
    assert_eq!(project.edges.len(), 0);

    let lawyer = lawyer_view(&pg, missing, 2).await.unwrap();
    assert_eq!(lawyer.nodes.len(), 0);

    let firm = firm_view(&pg, missing).await.unwrap();
    assert_eq!(firm.nodes.len(), 0);
    assert_eq!(firm.edges.len(), 0);
}
