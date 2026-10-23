use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::mem_surreal;

#[test]
fn project_service_mirror_key_names_only_its_row() {
    let id = Uuid::nil();
    let key = MirrorKey::ProjectService(id);
    assert_eq!(key.table(), "project_service");
    assert_eq!(key.to_string(), format!("project_service:{id}"));
}

#[test]
fn project_service_timestamps_are_required_and_truncated_to_postgres_precision() {
    assert_eq!(
        timestamp("2026-10-15T12:00:00.123456789Z")
            .unwrap()
            .timestamp_subsec_nanos(),
        123_456_000
    );
    assert!(matches!(
        timestamp("not a timestamp"),
        Err(MirrorError::SourceTimestamp)
    ));
}

#[tokio::test]
async fn project_service_mirrors_replay_and_reconcile() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let project = Uuid::now_v7();
    let document = Uuid::now_v7();
    let id = Uuid::now_v7();
    let letter = Uuid::now_v7();
    let (person, entity, sent_email) = create_source_service_parents(&surreal, project).await;
    insert_project_and_asset(&pg, project, document).await;
    create_source_service(
        &surreal,
        ServiceSource {
            id,
            project,
            document,
            entity,
            person,
            letter,
            sent_email,
        },
    )
    .await;

    let key = MirrorKey::ProjectService(id);
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());
    assert_full_service_shape(&pg, id, person, entity, letter, sent_email).await;

    surreal
        .query("UPDATE $id SET address='updated@example.test', updated_at='2026-10-16T12:00:00Z'")
        .bind(("id", crate::surreal::record_id("project_service", id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    sqlx::query("DELETE FROM project_service WHERE id=$1")
        .bind(id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let address: String = sqlx::query_scalar("SELECT address FROM project_service WHERE id=$1")
        .bind(id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(address, "updated@example.test");

    surreal
        .query("DELETE $id")
        .bind(("id", crate::surreal::record_id("project_service", id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM project_service WHERE id=$1")
        .bind(id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(
        rows, 0,
        "a source-deleted Service is removed from the mirror"
    );
    pg.0.close().await;
}

async fn create_source_service_parents(
    surreal: &crate::surreal::SurrealDb,
    project_id: Uuid,
) -> (Uuid, Uuid, Uuid) {
    let person = crate::persons::create(
        surreal,
        &crate::persons::NewPerson::with_role(
            "Synthetic declarant",
            format!("declarant-{}@example.test", Uuid::now_v7()),
            crate::persons::Role::Lawyer,
        ),
    )
    .await
    .unwrap()
    .id;
    let jurisdiction = crate::jurisdictions::create(
        surreal,
        &crate::jurisdictions::NewJurisdiction::new(
            format!("Synthetic service jurisdiction {}", Uuid::now_v7()),
            format!("S{}", &Uuid::now_v7().simple().to_string()[..7]),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = crate::entity_types::create(surreal, "Synthetic service entity type")
        .await
        .unwrap();
    let entity = crate::entities::create(
        surreal,
        &crate::entities::NewEntity {
            name: format!("Synthetic recipient {}", Uuid::now_v7()),
            entity_type_id: entity_type.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
    .id;
    let email = crate::sent_emails::record(
        surreal,
        &crate::sent_emails::NewSentEmail {
            recipient: "synthetic-recipient@example.test".into(),
            subject: "Synthetic service evidence".into(),
            sender: "synthetic-sender@example.test".into(),
            project_id: Some(project_id),
            template_slug: Some("synthetic-service".into()),
            body: "Synthetic audit content".into(),
            outcome: "captured".into(),
            sg_message_id: None,
            sent_at: "2026-10-15T12:00:00Z".parse().unwrap(),
        },
    )
    .await
    .unwrap();
    (person, entity, email.id)
}

async fn insert_project_and_asset(pg: &PostgresDb, project: Uuid, document: Uuid) {
    let jurisdiction = Uuid::now_v7();
    let entity_type = Uuid::now_v7();
    let entity = Uuid::now_v7();
    sqlx::query("INSERT INTO jurisdiction (id,name) VALUES ($1,'Synthetic project jurisdiction')")
        .bind(jurisdiction)
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic project entity type')")
        .bind(entity_type)
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic project entity',$2,$3)")
        .bind(entity).bind(entity_type).bind(jurisdiction).execute(&pg.0).await.unwrap();
    sqlx::query("INSERT INTO project (id,code,name,status,entity_id) VALUES ($1,$2,'Synthetic project','open',$3)")
        .bind(project).bind(format!("service-{project}")).bind(entity).execute(&pg.0).await.unwrap();
    sqlx::query("INSERT INTO asset (id,storage_key,content_type,byte_size,sha256_hex,project_id) VALUES ($1,$2,'application/pdf',1,'synthetic',$3)")
        .bind(document).bind(format!("synthetic/{document}.pdf")).bind(project).execute(&pg.0).await.unwrap();
}

struct ServiceSource {
    id: Uuid,
    project: Uuid,
    document: Uuid,
    entity: Uuid,
    person: Uuid,
    letter: Uuid,
    sent_email: Uuid,
}

async fn create_source_service(surreal: &crate::surreal::SurrealDb, source: ServiceSource) {
    surreal
        .query("CREATE $id SET project_id=$project, document_id=$document, recipient='Synthetic recipient', recipient_person_id=NONE, recipient_entity_id=$entity, counsel_for='Synthetic client', method='e_service', served_on='2026-10-15', address='service@example.test', declarant='Synthetic declarant', declarant_person_id=$person, letter_id=$letter, sent_email_id=$sent_email, inserted_at='2026-10-15T12:00:00.123456789Z', updated_at='2026-10-15T12:00:00.987654321Z'")
        .bind(("id", crate::surreal::record_id("project_service", source.id)))
        .bind(("project", crate::surreal::record_id("project", source.project)))
        .bind(("document", crate::surreal::record_id("asset", source.document)))
        .bind(("entity", crate::surreal::record_id("entity", source.entity)))
        .bind(("person", crate::surreal::record_id("person", source.person)))
        .bind(("letter", crate::surreal::record_id("letter", source.letter)))
        .bind(("sent_email", crate::surreal::record_id("sent_email", source.sent_email)))
        .await
        .unwrap()
        .check()
        .unwrap();
}

async fn assert_full_service_shape(
    pg: &PostgresDb,
    id: Uuid,
    person: Uuid,
    entity: Uuid,
    letter: Uuid,
    sent_email: Uuid,
) {
    let (inserted_at, updated_at): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT inserted_at, updated_at FROM project_service WHERE id=$1")
            .bind(id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(inserted_at.timestamp_subsec_nanos(), 123_456_000);
    assert_eq!(updated_at.timestamp_subsec_nanos(), 987_654_000);
    let (stored_entity, stored_letter, stored_email): (Option<Uuid>, Option<Uuid>, Option<Uuid>) =
        sqlx::query_as(
            "SELECT recipient_entity_id, letter_id, sent_email_id FROM project_service WHERE id=$1",
        )
        .bind(id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(stored_entity, Some(entity));
    assert_eq!(stored_letter, Some(letter));
    assert_eq!(stored_email, Some(sent_email));
    let person_mirrored: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM person WHERE id=$1)")
            .bind(person)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    let entity_mirrored: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM entity WHERE id=$1)")
            .bind(entity)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    let email_mirrored: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sent_email WHERE id=$1)")
            .bind(sent_email)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert!(person_mirrored, "expected on-demand Person mirror");
    assert!(entity_mirrored, "expected on-demand Entity mirror");
    assert!(email_mirrored, "expected on-demand sent-email mirror");
}

#[tokio::test]
async fn project_service_refuses_a_document_from_another_project() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let jurisdiction = Uuid::now_v7();
    let entity_type = Uuid::now_v7();
    let entity = Uuid::now_v7();
    let project = Uuid::now_v7();
    let other_project = Uuid::now_v7();
    let document = Uuid::now_v7();
    let id = Uuid::now_v7();

    sqlx::query("INSERT INTO jurisdiction (id,name) VALUES ($1,'Synthetic jurisdiction')")
        .bind(jurisdiction)
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic entity type')")
        .bind(entity_type)
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic entity',$2,$3)")
        .bind(entity).bind(entity_type).bind(jurisdiction).execute(&pg.0).await.unwrap();
    for (id, code) in [(project, "service-a"), (other_project, "service-b")] {
        sqlx::query("INSERT INTO project (id,code,name,status,entity_id) VALUES ($1,$2,'Synthetic project','open',$3)")
            .bind(id).bind(code).bind(entity).execute(&pg.0).await.unwrap();
    }
    sqlx::query("INSERT INTO asset (id,storage_key,content_type,byte_size,sha256_hex,project_id) VALUES ($1,$2,'application/pdf',1,'synthetic',$3)")
        .bind(document).bind(format!("synthetic/{document}.pdf")).bind(other_project).execute(&pg.0).await.unwrap();

    surreal
        .query("CREATE $id SET project_id=$project, document_id=$document, recipient='Synthetic recipient', method='mail', served_on='2026-10-15', address='service@example.test', declarant='Synthetic declarant', inserted_at='2026-10-15T12:00:00Z', updated_at='2026-10-15T12:00:00Z'")
        .bind(("id", crate::surreal::record_id("project_service", id)))
        .bind(("project", crate::surreal::record_id("project", project)))
        .bind(("document", crate::surreal::record_id("asset", document)))
        .await
        .unwrap()
        .check()
        .unwrap();

    assert!(matches!(
        mirror(&surreal, &pg, &MirrorKey::ProjectService(id)).await,
        Err(MirrorError::Source)
    ));
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM project_service WHERE id=$1")
        .bind(id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    pg.0.close().await;
}
