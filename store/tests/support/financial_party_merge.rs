//! Financial tagged-link relinking is relational; no graph convergence is inferred.
use serde_json::Value;
use store::{application_store::ApplicationStore, postgres::PostgresDb, project_references::Party};
use uuid::Uuid;

struct Fixture {
    project: Uuid,
    kept: Uuid,
    dropped: Uuid,
}

async fn holding(db: &ApplicationStore, project: Uuid, party: Party, class: &str) {
    store::cap_table_holdings::insert(
        db,
        project,
        &store::cap_table_holdings::CapTableHoldingFields {
            holder: party,
            share_class: class.into(),
            shares: 37,
            vesting_note: Some("Synthetic retained vesting and attribution".into()),
        },
    )
    .await
    .unwrap();
}

async fn fiduciary(db: &ApplicationStore, project: Uuid, party: Party, role: &str, order: i64) {
    store::fiduciary_roles::insert(
        db,
        project,
        &store::fiduciary_roles::FiduciaryRoleFields {
            fiduciary: party,
            role: role.into(),
            successor_order: order,
            notation_id: None,
        },
    )
    .await
    .unwrap();
}

async fn seed(pg: &PostgresDb) -> Fixture {
    let db = ApplicationStore::uninitialized();
    let project = super::native_project::create("synthetic-financial-tagged-merge").await;
    let mut people = Vec::new();
    for name in ["Synthetic kept party", "Synthetic dropped party"] {
        people.push(
            store::persons::create(
                &db,
                &store::persons::NewPerson::new(
                    name,
                    format!("financial-party-{}@example.test", Uuid::new_v4()),
                ),
            )
            .await
            .unwrap()
            .id,
        );
    }
    let (kept, dropped) = (people[0], people[1]);
    // A distinct canonical Entity with the same UUID witnesses typed isolation.
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) SELECT $1,'Synthetic same UUID financial Entity',entity_type_id,jurisdiction_id FROM entity WHERE id=$2")
        .bind(dropped).bind(project.entity_id).execute(pg.pool()).await.unwrap();
    holding(
        &db,
        project.id,
        Party::person(dropped),
        "synthetic person preferred",
    )
    .await;
    holding(
        &db,
        project.id,
        Party::entity(dropped),
        "synthetic entity preferred",
    )
    .await;
    fiduciary(&db, project.id, Party::person(dropped), "trustee", 1).await;
    fiduciary(&db, project.id, Party::entity(dropped), "executor", 2).await;
    Fixture {
        project: project.id,
        kept,
        dropped,
    }
}

async fn rows(pg: &PostgresDb, table: &str, project: Uuid) -> Vec<Value> {
    // Callers below use only the two fixed financial table names.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT to_jsonb(r) FROM {table} r WHERE project_id=$1 ORDER BY id"
    )))
    .bind(project)
    .fetch_all(pg.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn financial_person_tags_move_without_changing_same_uuid_entity_tags_or_row_metadata() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = ApplicationStore::uninitialized();
        let fixture = seed(&pg).await;
        let mut before = Vec::new();
        for table in ["cap_table_holding", "fiduciary_role"] {
            before.push(rows(&pg, table, fixture.project).await);
        }
        store::person_merge::merge(&db, fixture.kept, fixture.dropped)
            .await
            .unwrap();
        for ((table, column), mut expected) in [
            ("cap_table_holding", "holder_id"),
            ("fiduciary_role", "fiduciary_id"),
        ]
        .into_iter()
        .zip(before)
        {
            assert_eq!(expected.len(), 2);
            let mut moved = 0;
            let mut retained = 0;
            for row in &mut expected {
                assert_eq!(row[column]["id"]["Uuid"], fixture.dropped.to_string());
                if row[column]["tb"] == "person" {
                    row[column]["id"]["Uuid"] = Value::String(fixture.kept.to_string());
                    moved += 1;
                } else {
                    assert_eq!(row[column]["tb"], "entity");
                    retained += 1;
                }
            }
            assert_eq!((moved, retained), (1, 1));
            assert_eq!(
                rows(&pg, table, fixture.project).await,
                expected,
                "only the Person UUID changes; IDs, timestamps and financial metadata survive"
            );
        }
        assert!(store::persons::find_by_id(&db, fixture.dropped)
            .await
            .unwrap()
            .is_none());
        assert!(store::persons::find_by_id(&db, fixture.kept)
            .await
            .unwrap()
            .is_some());
    })
    .await;
}

#[tokio::test]
async fn colliding_financial_person_lines_refuse_merge_and_roll_back_affected_rows() {
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let db = ApplicationStore::uninitialized();
        let fixture = seed(&pg).await;
        holding(&db, fixture.project, Party::person(fixture.kept), "synthetic person preferred").await;
        let before = super::merge_fixture::snapshot(&pg).await;
        let error = store::person_merge::merge(&db, fixture.kept, fixture.dropped).await;
        assert!(matches!(error, Err(store::person_merge::PersonMergeError::Person(
            store::persons::PersonError::Postgres(_)))));
        assert_eq!(super::merge_fixture::snapshot(&pg).await, before,
            "collision must roll back mailbox moves, financial tags and all other canonical changes");
    }).await;
}
