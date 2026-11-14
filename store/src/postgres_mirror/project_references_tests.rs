use super::*;
use crate::persons::{NewPerson, Role};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::project_references::{Party, REASON_COUNTERPARTY_ENTITY, REASON_COUNTERPARTY_OFFICER};
use crate::test_support::mem_surreal;

fn key(project_id: Uuid, kind: Endpoint, party_id: Uuid, reason: &str) -> MirrorKey {
    MirrorKey::ProjectReference {
        project_id,
        party_kind: kind.table().into(),
        party_id,
        reason: reason.into(),
    }
}

async fn source_entity(surreal: &SurrealDb) -> Uuid {
    crate::postgres_mirror::tests::synthetic_entity(surreal).await
}

async fn source_project(surreal: &SurrealDb) -> Uuid {
    let entity_id = source_entity(surreal).await;
    let id = Uuid::now_v7();
    crate::projects::create(
        surreal,
        &crate::projects::NewProject {
            code: crate::projects::code_from_name("Synthetic reference matter", id),
            name: "Synthetic reference matter".into(),
            status: "open".into(),
            entity_id,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

#[test]
fn keys_are_natural_and_never_include_party_values() {
    let id = Uuid::nil();
    let key = key(id, Endpoint::Entity, id, REASON_COUNTERPARTY_ENTITY);
    assert_eq!(key.table(), "project_reference");
    assert_eq!(
        key.to_string(),
        "project_reference:00000000-0000-0000-0000-000000000000/entity:00000000-0000-0000-0000-000000000000/counterparty_entity"
    );
    assert_eq!(upsert_sql(Endpoint::Person).matches('$').count(), 5);
    assert_eq!(upsert_sql(Endpoint::Entity).matches('$').count(), 5);
}

#[test]
fn canonical_schema_contains_project_reference_storage() {
    assert!(
        crate::postgres_schema::canonical_schema().contains("CREATE TABLE project_reference ("),
        "the canonical PostgreSQL schema includes Project Reference storage"
    );
}

#[tokio::test]
async fn both_endpoint_kinds_converge_and_source_deletion_removes_the_mirror() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let rows = fixture(&surreal).await;

    assert_mirrors(&surreal, &pg, &rows).await;
    assert_reconcile_repairs_drift(&surreal, &pg, &rows).await;
    delete_source_person_reference(&surreal, &pg, &rows).await;
}

struct Fixture {
    project_id: Uuid,
    person_id: Uuid,
    person_reference: crate::project_references::ProjectReference,
    person_key: MirrorKey,
    entity_key: MirrorKey,
}

async fn fixture(surreal: &SurrealDb) -> Fixture {
    let project_id = source_project(surreal).await;
    let entity_id = source_entity(surreal).await;
    let person = crate::persons::create(
        surreal,
        &NewPerson::with_role(
            "Synthetic counterparty officer",
            format!("reference-{}@example.com", Uuid::now_v7()),
            Role::Client,
        ),
    )
    .await
    .unwrap()
    .id;
    let person_reference = crate::project_references::reference(
        surreal,
        project_id,
        Party::person(person),
        REASON_COUNTERPARTY_OFFICER,
    )
    .await
    .unwrap();
    crate::project_references::reference(
        surreal,
        project_id,
        Party::entity(entity_id),
        REASON_COUNTERPARTY_ENTITY,
    )
    .await
    .unwrap();
    Fixture {
        project_id,
        person_id: person,
        person_reference,
        person_key: key(
            project_id,
            Endpoint::Person,
            person,
            REASON_COUNTERPARTY_OFFICER,
        ),
        entity_key: key(
            project_id,
            Endpoint::Entity,
            entity_id,
            REASON_COUNTERPARTY_ENTITY,
        ),
    }
}

async fn assert_mirrors(surreal: &SurrealDb, pg: &PostgresDb, rows: &Fixture) {
    assert_eq!(
        mirror(surreal, pg, &rows.person_key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(surreal, pg, &rows.person_key).await.unwrap(),
        Mirrored::Unchanged
    );
    assert_eq!(
        mirror(surreal, pg, &rows.entity_key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(surreal, pg, &rows.entity_key).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(parity(surreal, pg).await.unwrap().is_clean());
}

async fn assert_reconcile_repairs_drift(surreal: &SurrealDb, pg: &PostgresDb, rows: &Fixture) {
    sqlx::query("DELETE FROM project_reference WHERE person_id = $1")
        .bind(rows.person_id)
        .execute(&pg.0)
        .await
        .unwrap();
    let drift = parity(surreal, pg).await.unwrap();
    assert_eq!(drift.tables[0].divergent, vec![rows.person_key.clone()]);
    assert!(reconcile(surreal, pg).await.unwrap().is_clean());
}

async fn delete_source_person_reference(surreal: &SurrealDb, pg: &PostgresDb, rows: &Fixture) {
    surreal
        .query("DELETE project_reference WHERE in = $project AND out = $party AND reason = $reason")
        .bind(("project", record_id("project", rows.project_id)))
        .bind(("party", record_id("person", rows.person_id)))
        .bind(("reason", rows.person_reference.reason.clone()))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        mirror(surreal, pg, &rows.person_key).await.unwrap(),
        Mirrored::Deleted
    );
    assert_eq!(
        mirror(surreal, pg, &rows.entity_key).await.unwrap(),
        Mirrored::Unchanged
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM project_reference")
            .fetch_one(&pg.0)
            .await
            .unwrap(),
        1
    );
    assert!(parity(surreal, pg).await.unwrap().is_clean());
}

/// Wherever PostgreSQL is configured it holds every graph table:
/// references, structural ties, the adversity write's log row and edge, and
/// the Relationship Log reads all land in and answer from PostgreSQL,
/// SurrealDB receives nothing, and the mirror copies Surreal-era rows but
/// never lets a row SurrealDB lacks delete an authoritative one.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one cutover walks every graph table's routed write and read and the mirror guard"
)]
async fn the_age_engine_holds_the_graph_tables_and_the_mirror_stands_down() {
    use crate::postgres_graph::with_test_authority;
    use crate::project_references::{
        find, for_project, is_referenced, record_adversity, reference, AdversitySubject,
        RecordAdversityCommand, ADVERSITY_RECORDED_ACTION, REASON_SIGNATORY,
    };
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    // The Surreal-era rows, mirrored before the cutover: PostgreSQL now
    // holds the Project, the Person, the Entity, and both references.
    let rows = fixture(&surreal).await;
    assert_mirrors(&surreal, &pg, &rows).await;
    let MirrorKey::ProjectReference {
        party_id: entity_id,
        ..
    } = rows.entity_key.clone()
    else {
        unreachable!("the fixture keys a project reference");
    };
    let held = Some(pg.clone());

    with_test_authority(held.clone(), async {
        // A Surreal-era row still copies across, but a row SurrealDB lacks
        // deletes nothing PostgreSQL holds.
        assert_eq!(
            mirror(&surreal, &pg, &rows.entity_key).await.unwrap(),
            Mirrored::Unchanged
        );
        surreal
            .query("DELETE project_reference WHERE out = $party")
            .bind(("party", record_id("person", rows.person_id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(matches!(
            mirror(&surreal, &pg, &rows.person_key).await,
            Err(MirrorError::GraphAuthority)
        ));
        assert!(crate::postgres_graph::holds("relationship_log").await);

        // References write and read PostgreSQL only, idempotently.
        let signatory = Party::person(rows.person_id);
        let first = reference(&surreal, rows.project_id, signatory, REASON_SIGNATORY)
            .await
            .unwrap();
        let again = reference(&surreal, rows.project_id, signatory, REASON_SIGNATORY)
            .await
            .unwrap();
        assert_eq!(first, again, "the second write reads the first row back");
        assert_eq!(
            find(&surreal, rows.project_id, signatory, REASON_SIGNATORY)
                .await
                .unwrap(),
            Some(first)
        );
        assert_eq!(
            for_project(&surreal, rows.project_id).await.unwrap().len(),
            3
        );
        assert!(is_referenced(&surreal, rows.project_id, signatory)
            .await
            .unwrap());

        // Structural ties read back on both ends from PostgreSQL.
        let tie = crate::entity_roles::grant(&surreal, rows.person_id, entity_id, "manages")
            .await
            .unwrap();
        assert_eq!(
            crate::entity_roles::find(&surreal, rows.person_id, entity_id, "manages")
                .await
                .unwrap(),
            Some(tie.clone())
        );
        assert_eq!(
            crate::entity_roles::for_person(&surreal, rows.person_id)
                .await
                .unwrap(),
            vec![tie.clone()]
        );
        assert_eq!(
            crate::entity_roles::for_entity(&surreal, entity_id)
                .await
                .unwrap(),
            vec![tie.clone()]
        );
        assert_eq!(crate::entity_roles::all(&surreal).await.unwrap(), vec![tie]);

        // Adversity appends its log row and sources its edge to it.
        let edge = record_adversity(
            &surreal,
            &RecordAdversityCommand {
                project_id: rows.project_id,
                party: Party::entity(entity_id),
                subject: AdversitySubject::Entity,
                actor_person_id: Some(rows.person_id),
            },
        )
        .await
        .unwrap();
        let log = crate::relationship_logs::for_project(&surreal, rows.project_id)
            .await
            .unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].action, ADVERSITY_RECORDED_ACTION);
        assert_eq!(log[0].actor_person_id, Some(rows.person_id));
        assert_eq!(edge.source_id, Some(log[0].id));
        assert_eq!(
            crate::relationships::all(&surreal).await.unwrap(),
            vec![edge]
        );
        let entry = crate::relationship_logs::NewRelationshipLog {
            actor_person_id: Some(rows.person_id),
            subject_type: "project".into(),
            subject_id: rows.project_id,
            action: ADVERSITY_RECORDED_ACTION.into(),
            detail: log[0].detail.clone(),
            project_id: Some(rows.project_id),
        };
        assert!(crate::relationship_logs::exists(&surreal, &entry)
            .await
            .unwrap());
        assert_eq!(
            crate::relationship_logs::for_subject(&surreal, "project", rows.project_id)
                .await
                .unwrap(),
            log
        );
        assert_eq!(crate::relationship_logs::all(&surreal).await.unwrap(), log);
    })
    .await;

    // SurrealDB received none of it; the AGE graph took both edges.
    with_test_authority(None, async {
        assert_eq!(
            for_project(&surreal, rows.project_id).await.unwrap().len(),
            1
        );
        assert!(crate::entity_roles::all(&surreal).await.unwrap().is_empty());
        assert!(crate::relationships::all(&surreal)
            .await
            .unwrap()
            .is_empty());
        assert!(crate::relationship_logs::all(&surreal)
            .await
            .unwrap()
            .is_empty());
    })
    .await;
    let edges = sqlx::query("SELECT t.v::text FROM graph_conflict_edges($1) AS t(v)")
        .bind(serde_json::json!([entity_id.to_string()]).to_string())
        .fetch_all(&pg.0)
        .await
        .unwrap();
    assert_eq!(edges.len(), 2, "HAS_ROLE and the adverse RELATIONSHIP");
}
