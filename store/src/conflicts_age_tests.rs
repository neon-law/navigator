//! The conflict check's findings-level parity: the same staged graph, read
//! through the Surreal engine and the AGE engine, must produce the *same*
//! conflict report — severities, reasons, counterparties, explanations,
//! and confidences — not merely the same edge set. The edge-set comparison
//! (`postgres_graph_tests::traversal_parity_holds_on_shared_fixtures`)
//! proves both engines walk the same graph; this file proves the walk's
//! *findings* agree, which is what a cutover actually risks.
//!
//! Fixtures live in both stores under the same record UUIDs and names: the
//! graph edges seed through the store seams into Surreal and through
//! direct inserts into the PostgreSQL mirror, the projection loads, and
//! each scenario runs [`build_graph_with_engines`] twice — once per
//! engine — over the same Surreal lookups. What differs between the two
//! runs is the traversal engine and nothing else: the scoring, the
//! floors, and the finding shapes are the same Rust for both.
//!
//! These tests self-skip without `NAVIGATOR_TEST_POSTGRES_PORT` (see
//! `postgres_mirror::tests`), and need the AGE-enabled image; CI's
//! coverage job arms both.

use super::{build_graph_with_engines, ConflictReport, GraphEngine, Reason, Severity};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::relationships::{Endpoint, NewRelationship, KIND_ADVERSE_TO, SOURCE_MANUAL};
use crate::test_support::{mem_surreal, SEED_ENTITY_JURISDICTION_ID, SEED_ENTITY_TYPE_ID};
use crate::{postgres::PostgresDb, surreal::SurrealDb};

/// The fixed insert timestamp every fixture row carries.
fn fixture_time() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc)
}

/// The reference rows and the entity, in both stores, under the same UUIDs
/// and names. The entity is created through the Surreal seam and inserted
/// into PostgreSQL with the same name; the `entity_type` and `jurisdiction`
/// reference rows exist in PostgreSQL only — the AGE projection reads their
/// names there, and the Surreal engine never reads them. Seeding those rows
/// in Surreal too would collide on the fixed seed IDs: CI's Surreal
/// container is shared across test binaries, so a plain `CREATE` fails once
/// any earlier binary has made the record.
async fn seed_entity_both(surreal: &SurrealDb, pg: &PostgresDb) -> uuid::Uuid {
    let inserted_at = fixture_time();

    let entity = crate::entities::create(
        surreal,
        &crate::entities::NewEntity {
            name: "Synthetic Entity".into(),
            entity_type_id: SEED_ENTITY_TYPE_ID,
            jurisdiction_id: SEED_ENTITY_JURISDICTION_ID,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
    .id;

    // The test container is shared across binaries: reference rows are
    // seed-once, so a duplicate insert is a no-op, not a failure.
    sqlx::query(
        "INSERT INTO entity_type (id, name, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Entity Type', $2, $2) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(SEED_ENTITY_TYPE_ID)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jurisdiction (id, name, code, jurisdiction_type, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Jurisdiction', 'ZZ', 'state', $2, $2) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(SEED_ENTITY_JURISDICTION_ID)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO entity (id, name, entity_type_id, jurisdiction_id, inserted_at, updated_at) \
         VALUES ($1, 'Synthetic Entity', $2, $3, $4, $4)",
    )
    .bind(entity)
    .bind(SEED_ENTITY_TYPE_ID)
    .bind(SEED_ENTITY_JURISDICTION_ID)
    .bind(inserted_at)
    .execute(&pg.0)
    .await
    .unwrap();
    entity
}

/// One person, in both stores, under the same UUID and name — the name is
/// what a finding prints, so it has to agree across engines.
async fn person_both(surreal: &SurrealDb, pg: &PostgresDb, name: &str) -> uuid::Uuid {
    let id = crate::persons::create(
        surreal,
        &crate::persons::NewPerson::new(name, format!("{}@example.com", uuid::Uuid::now_v7())),
    )
    .await
    .unwrap()
    .id;
    sqlx::query(
        "INSERT INTO person (id, name, email, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $4)",
    )
    .bind(id)
    .bind(name)
    .bind(format!("{id}@example.com"))
    .bind(fixture_time())
    .execute(&pg.0)
    .await
    .unwrap();
    id
}

/// One structural tie, seeded through the Surreal seam and inserted into
/// PostgreSQL — the projection reads the mirror row, Surreal reads its own.
async fn role_both(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    person: uuid::Uuid,
    entity: uuid::Uuid,
    role: &str,
) {
    crate::entity_roles::grant(surreal, person, entity, role)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO entity_role (person_id, entity_id, role, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $4)",
    )
    .bind(person)
    .bind(entity)
    .bind(role)
    .bind(fixture_time())
    .execute(&pg.0)
    .await
    .unwrap();
}

/// One typed relationship, seeded the same way. `source_id` stays null —
/// the graph edge does not need provenance for the traversal to agree.
async fn edge_both(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    from: (Endpoint, uuid::Uuid),
    to: (Endpoint, uuid::Uuid),
    kind: &str,
    confidence_pct: i32,
) {
    crate::relationships::record(
        surreal,
        &NewRelationship {
            from: from.0,
            from_id: from.1,
            to: to.0,
            to_id: to.1,
            kind: kind.into(),
            confidence_pct,
            source_kind: SOURCE_MANUAL.into(),
            source_id: None,
            detail: None,
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO \"relationship\" (id, \"in\", \"out\", kind, confidence_pct, source_kind, \
         inserted_at, updated_at) \
         VALUES ($1, $2::jsonb, $3::jsonb, $4, $5, 'manual', $6, $6)",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(serde_json::json!({"tb": from.0.table(), "id": {"Uuid": from.1.to_string()}}).to_string())
    .bind(serde_json::json!({"tb": to.0.table(), "id": {"Uuid": to.1.to_string()}}).to_string())
    .bind(kind)
    .bind(i64::from(confidence_pct))
    .bind(fixture_time())
    .execute(&pg.0)
    .await
    .unwrap();
}

/// One open matter for `client` on `entity`, in Surreal only: the conflict
/// check's client sets come from the Surreal participation markers, which
/// are not graph state. Returns the matter's id.
async fn open_project(
    surreal: &SurrealDb,
    entity_id: uuid::Uuid,
    client_id: uuid::Uuid,
) -> uuid::Uuid {
    let project = crate::projects::create(
        surreal,
        &crate::projects::NewProject {
            code: format!("existing-matter-{}", uuid::Uuid::now_v7()),
            name: "Existing matter".into(),
            status: "open".into(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    crate::projects::designate_dri_in_surreal(
        surreal,
        project.id,
        client_id,
        crate::projects::DriSide::Client,
    )
    .await
    .unwrap();
    crate::projects::designate_dri_in_surreal(
        surreal,
        project.id,
        crate::test_support::dri_person(surreal).await,
        crate::projects::DriSide::Lawyer,
    )
    .await
    .unwrap();
    project.id
}

/// The pre-matter check run through both engines over the same stores, the
/// projection loaded first so the AGE engine reads a current graph.
async fn both_reports(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    client_id: uuid::Uuid,
    entity_id: uuid::Uuid,
) -> (ConflictReport, ConflictReport) {
    crate::postgres_graph::project_graph(pg).await.unwrap();
    let surreal_engine = build_graph_with_engines(surreal, GraphEngine::Surreal, None)
        .await
        .unwrap();
    let age_engine = build_graph_with_engines(surreal, GraphEngine::Age, Some(pg.clone()))
        .await
        .unwrap();
    let surreal_report = surreal_engine.check(client_id, entity_id).await.unwrap();
    let age_report = age_engine.check(client_id, entity_id).await.unwrap();
    (surreal_report, age_report)
}

#[tokio::test]
async fn a_clear_graph_is_clear_through_both_engines() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let entity = seed_entity_both(&surreal, &pg).await;
    let client = person_both(&surreal, &pg, "Fresh Client").await;

    let (surreal_report, age_report) = both_reports(&surreal, &pg, client, entity).await;
    assert!(
        surreal_report.is_clear(),
        "surreal findings: {:?}",
        surreal_report.summary_lines()
    );
    assert_eq!(surreal_report, age_report, "both engines see a clear graph");
}

#[tokio::test]
async fn direct_adversity_blocks_through_both_engines() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let entity = seed_entity_both(&surreal, &pg).await;
    let opponent = person_both(&surreal, &pg, "Opposing Party").await;
    let proposed = person_both(&surreal, &pg, "Proposed Client").await;
    open_project(&surreal, entity, opponent).await;
    edge_both(
        &surreal,
        &pg,
        (Endpoint::Person, proposed),
        (Endpoint::Person, opponent),
        KIND_ADVERSE_TO,
        100,
    )
    .await;

    let (surreal_report, age_report) = both_reports(&surreal, &pg, proposed, entity).await;
    assert_eq!(
        surreal_report,
        age_report,
        "both engines block the same way: {:?} vs {:?}",
        surreal_report.summary_lines(),
        age_report.summary_lines()
    );
    // The opponent's own open matter on the entity is a legitimate second
    // finding: the entity surfaces as a shared party beside the direct
    // adversity, and the parity assert above proves both engines see the
    // same two.
    assert_eq!(
        age_report.findings.len(),
        2,
        "the direct finding plus the shared party"
    );
    assert!(
        age_report.has_blocking(),
        "a direct, confident adversity blocks"
    );
    // The counterparty prints the name the projection carried, not a bare
    // id — the name projection is part of the read shape.
    let adverse = age_report
        .findings
        .iter()
        .find(|f| f.reason == Reason::Adverse)
        .expect("the direct adversity finding");
    assert_eq!(adverse.counterparty, "Opposing Party");
    assert_eq!(adverse.severity, Severity::Block);
    assert!(age_report
        .findings
        .iter()
        .any(|f| f.reason == Reason::SharedParty));
}

#[tokio::test]
async fn low_confidence_adversity_reviews_through_both_engines() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let entity = seed_entity_both(&surreal, &pg).await;
    let opponent = person_both(&surreal, &pg, "Low Confidence Opponent").await;
    let proposed = person_both(&surreal, &pg, "Proposed Client").await;
    open_project(&surreal, entity, opponent).await;
    edge_both(
        &surreal,
        &pg,
        (Endpoint::Person, proposed),
        (Endpoint::Person, opponent),
        KIND_ADVERSE_TO,
        40,
    )
    .await;

    let (surreal_report, age_report) = both_reports(&surreal, &pg, proposed, entity).await;
    assert_eq!(
        surreal_report,
        age_report,
        "both engines downgrade the same way: {:?} vs {:?}",
        surreal_report.summary_lines(),
        age_report.summary_lines()
    );
    assert!(!age_report.has_blocking(), "a shaky adversity cannot block");
    // The shared party beside the adversity carries full confidence; the
    // adverse finding itself is the 40% review.
    let adverse = age_report
        .findings
        .iter()
        .find(|f| f.reason == Reason::Adverse)
        .expect("the low-confidence adversity finding");
    assert_eq!(adverse.confidence_pct, 40);
    assert_eq!(adverse.severity, Severity::Review);
    assert!(age_report
        .findings
        .iter()
        .any(|f| f.reason == Reason::SharedParty));
}

#[tokio::test]
async fn structural_ties_agree_across_engines() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let shared = seed_entity_both(&surreal, &pg).await;
    let beneficiary = person_both(&surreal, &pg, "Beneficiary Client").await;
    let proposed = person_both(&surreal, &pg, "Proposed Client").await;
    // The beneficiary is a current client of the firm on the shared
    // entity, and the proposed client holds a structural role in it: the
    // check anchors on the proposed client and the shared entity, and the
    // entity itself is already a party the firm serves — the anchor at
    // hops 0 — while the role tie reaches the beneficiary at hops 2.
    open_project(&surreal, shared, beneficiary).await;
    role_both(&surreal, &pg, proposed, shared, "manages").await;
    role_both(&surreal, &pg, beneficiary, shared, "beneficiary").await;

    let (surreal_report, age_report) = both_reports(&surreal, &pg, proposed, shared).await;
    assert_eq!(
        surreal_report,
        age_report,
        "both engines walk the structural path the same way: {:?} vs {:?}",
        surreal_report.summary_lines(),
        age_report.summary_lines()
    );
    // The shared entity (hops 0) and the beneficiary (hops 2, through the
    // two role ties) are each a SharedParty review — no adversity anywhere,
    // so nothing blocks.
    assert!(!age_report.has_blocking());
    assert_eq!(age_report.findings.len(), 2);
    assert!(age_report
        .findings
        .iter()
        .all(|f| f.reason == Reason::SharedParty && f.severity == Severity::Review));
    assert!(age_report
        .findings
        .iter()
        .any(|f| f.counterparty == "Beneficiary Client"));
}

#[tokio::test]
async fn dashboard_scan_agrees_across_engines() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let entity = seed_entity_both(&surreal, &pg).await;
    let beneficiary = person_both(&surreal, &pg, "Beneficiary Client").await;
    // The scanned matter: the beneficiary's own.
    let scanned = open_project(&surreal, entity, beneficiary).await;
    // A second client of the firm holds a structural role in the scanned
    // matter's entity — the shared-party shape the dashboard exists to
    // catch, reached through the graph rather than through the anchor's
    // own client set.
    let second_client = person_both(&surreal, &pg, "Second Client").await;
    let other_entity = seed_entity_both(&surreal, &pg).await;
    open_project(&surreal, other_entity, second_client).await;
    role_both(&surreal, &pg, second_client, entity, "beneficiary").await;

    crate::postgres_graph::project_graph(&pg).await.unwrap();
    let surreal_engine = build_graph_with_engines(&surreal, GraphEngine::Surreal, None)
        .await
        .unwrap();
    let age_engine = build_graph_with_engines(&surreal, GraphEngine::Age, Some(pg.clone()))
        .await
        .unwrap();
    let surreal_findings = surreal_engine
        .findings_for_projects(&[scanned])
        .await
        .unwrap();
    let age_findings = age_engine.findings_for_projects(&[scanned]).await.unwrap();
    assert_eq!(
        surreal_findings, age_findings,
        "both engines scan the dashboard the same way: {surreal_findings:?} vs {age_findings:?}"
    );
    assert!(
        age_findings
            .iter()
            .any(|f| f.reason == Reason::SharedParty && f.counterparty == "Second Client"),
        "the second client's tie surfaces as a shared-party review: {age_findings:?}"
    );
}

#[test]
fn the_engine_flag_parses_its_two_values() {
    use crate::postgres_graph::parse_graph_engine;
    assert_eq!(parse_graph_engine("surreal"), Ok(GraphEngine::Surreal));
    assert_eq!(parse_graph_engine("AGE"), Ok(GraphEngine::Age));
    assert_eq!(parse_graph_engine(" age "), Ok(GraphEngine::Age));
    assert!(parse_graph_engine("postgres").is_err());
    assert!(parse_graph_engine("").is_err());
}
