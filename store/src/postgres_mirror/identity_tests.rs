use super::*;
use crate::firms::NewFirm;
use crate::persons::{NewPerson, PersonEdit, Role};
use crate::postgres_mirror::tests::{connect, fresh_schema, synthetic_entity};
use crate::test_support::mem_surreal;

async fn person(surreal: &SurrealDb, role: Role) -> Uuid {
    crate::persons::create(
        surreal,
        &NewPerson::with_role(
            "Synthetic Person",
            format!("identity-{}@example.com", Uuid::now_v7()),
            role,
        ),
    )
    .await
    .unwrap()
    .id
}

async fn mirrored(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) -> Mirrored {
    super::super::mirror(surreal, pg, key).await.unwrap()
}

fn rows(report: &ParityReport) -> Vec<usize> {
    report
        .tables
        .iter()
        .map(|table| table.target_rows)
        .collect()
}

#[test]
fn identity_keys_name_rows_without_values() {
    let id = Uuid::nil();
    assert_eq!(MirrorKey::Person(id).table(), "person");
    assert_eq!(MirrorKey::Entity(id).table(), "entity");
    assert_eq!(MirrorKey::EntityType(id).table(), "entity_type");
    assert_eq!(MirrorKey::Jurisdiction(id).table(), "jurisdiction");
    assert_eq!(
        MirrorKey::Person(id).to_string(),
        "person:00000000-0000-0000-0000-000000000000"
    );
}

#[test]
fn the_person_upsert_leaves_the_generated_email_key_to_postgres() {
    let sql = upsert_sql("person", PERSON_COLUMNS);
    assert!(!sql.contains("email_lower"));
    assert!(sql.contains("is_admitted = EXCLUDED.is_admitted"));
}

#[tokio::test]
async fn person_and_entity_rows_converge_through_their_lifecycle() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;

    // An Entity brings its Entity Type and Jurisdiction with it.
    let entity = synthetic_entity(&surreal).await;
    let entity_key = MirrorKey::Entity(entity);
    assert_eq!(
        mirrored(&surreal, &pg, &entity_key).await,
        Mirrored::Written
    );
    assert_eq!(
        mirrored(&surreal, &pg, &entity_key).await,
        Mirrored::Unchanged
    );
    crate::entities::set_avatar_url(&surreal, entity, Some("/synthetic/avatar".into()))
        .await
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &entity_key).await,
        Mirrored::Written
    );

    // A Person, edited and withdrawn from sign-in.
    let client = person(&surreal, Role::Client).await;
    let person_key = MirrorKey::Person(client);
    assert_eq!(
        mirrored(&surreal, &pg, &person_key).await,
        Mirrored::Written
    );
    assert_eq!(
        mirrored(&surreal, &pg, &person_key).await,
        Mirrored::Unchanged
    );
    crate::persons::edit(
        &surreal,
        client,
        &PersonEdit {
            family_name: Some(Some("Synthetic".into())),
            ..PersonEdit::default()
        },
    )
    .await
    .unwrap();
    crate::persons::set_admitted(&surreal, client, false)
        .await
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &person_key).await,
        Mirrored::Written
    );
    let (family, admitted): (Option<String>, bool) =
        sqlx::query_as("SELECT family_name, is_admitted FROM person WHERE id = $1")
            .bind(client)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!((family.as_deref(), admitted), (Some("Synthetic"), false));

    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(rows(&report), [1, 1, 1, 1]);

    // Removal.
    crate::persons::delete(&surreal, client).await.unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &person_key).await,
        Mirrored::Deleted
    );
    crate::entities::delete_unless_firm_anchor(&surreal, entity)
        .await
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &entity_key).await,
        Mirrored::Deleted
    );
    assert_eq!(
        mirrored(&surreal, &pg, &entity_key).await,
        Mirrored::Unchanged
    );
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(rows(&report), [1, 1, 0, 0]);
}

#[tokio::test]
async fn reconcile_converges_identity_and_tenancy_stops_flagging_it() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    let entity = synthetic_entity(&surreal).await;
    let admin = person(&surreal, Role::Admin).await;
    let bystander = person(&surreal, Role::Client).await;
    let firm = crate::firms::create(
        &surreal,
        &NewFirm {
            name: "Synthetic Reconciled Practice".into(),
            status: "active".into(),
            entity_id: entity,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap();

    // PostgreSQL holds a Person SurrealDB does not, and none of the rest.
    let stale = Uuid::new_v4();
    sqlx::query("INSERT INTO person (id, name, email) VALUES ($1, 'Stale Person', $2)")
        .bind(stale)
        .bind(format!("{stale}@example.com"))
        .execute(&pg.0)
        .await
        .unwrap();
    let report = parity(&surreal, &pg).await.unwrap();
    let divergent: Vec<_> = report.divergent().cloned().collect();
    assert_eq!(divergent.len(), 6, "{divergent:?}");
    assert!(divergent.contains(&MirrorKey::Person(stale)));
    assert!(divergent.contains(&MirrorKey::Person(bystander)));
    // The report carries keys and counts, never a name or an email.
    assert!(!format!("{report:?}").contains("example.com"));

    // Tenancy reconciles on its own: each row it mirrors brings the Person
    // or Entity it names.
    let tenancy = super::super::tenancy::reconcile(&surreal, &pg)
        .await
        .unwrap();
    assert!(tenancy.is_clean(), "{tenancy:?}");
    assert_eq!(rows(&tenancy), [1, 0, 0, 0, 1, 0]);

    // Identity reconcile settles the rest.
    let report = reconcile(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(rows(&report), [1, 1, 1, 2]);
    let replay = reconcile(&surreal, &pg).await.unwrap();
    assert_eq!(replay, report);
    let firm_row: Option<Uuid> = sqlx::query_scalar("SELECT entity_id FROM firm WHERE id = $1")
        .bind(firm.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(firm_row, Some(entity));
}

#[tokio::test]
async fn an_entity_naming_a_missing_reference_is_refused_and_reported() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    // `seed_entity` names an Entity Type and Jurisdiction no SurrealDB holds.
    let entity = crate::test_support::seed_entity(&surreal).await;
    let error = super::super::mirror(&surreal, &pg, &MirrorKey::Entity(entity))
        .await
        .unwrap_err();
    assert!(matches!(error, MirrorError::Target(ref code) if code == "23503"));
    let report = reconcile(&surreal, &pg).await.unwrap();
    assert_eq!(report.tables[2].divergent, [MirrorKey::Entity(entity)]);
    assert_eq!(report.tables[2].target_rows, 0);
}
