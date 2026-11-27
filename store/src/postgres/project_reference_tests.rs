use crate::application_store::ApplicationStore;
use crate::postgres::fixtures::{connect, fresh_schema};
use crate::postgres_graph::with_test_authority;
use crate::project_references::{reference, Party};
use sqlx::Row;
use uuid::Uuid;

async fn matter(pg: &crate::postgres::PostgresDb) -> (Uuid, Uuid) {
    let jurisdiction = Uuid::new_v4();
    let entity_type = Uuid::new_v4();
    let entity = Uuid::new_v4();
    let project = Uuid::new_v4();
    let person = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Jurisdiction','TST')",
    )
    .bind(jurisdiction)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Entity')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)",
    )
    .bind(entity)
    .bind(entity_type)
    .bind(jurisdiction)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO project (id,code,name,status,entity_id) VALUES ($1,'synthetic-official','Synthetic Project','open',$2)",
    )
    .bind(project)
    .bind(entity)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO person (id,name,email,role) VALUES ($1,'Agency Auditor','auditor@example.com','client')",
    )
    .bind(person)
    .execute(pg.pool())
    .await
    .unwrap();
    (project, person)
}

#[tokio::test]
async fn an_existing_person_is_referenced_as_a_government_official() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (project, person) = matter(&pg).await;
    let idle = ApplicationStore::uninitialized();

    with_test_authority(Some(pg.clone()), async {
        let written = reference(&idle, project, Party::person(person), "government_official")
            .await
            .expect("an existing person is a government official");
        assert_eq!(written.reason, "government_official");
        assert_eq!(written.party, Party::person(person));
        let again = reference(&idle, project, Party::person(person), "government_official")
            .await
            .unwrap();
        assert_eq!(again, written);
    })
    .await;

    let reason: String = sqlx::query(
        "SELECT reason FROM project_reference WHERE project_id = $1 AND person_id = $2",
    )
    .bind(project)
    .bind(person)
    .fetch_one(pg.pool())
    .await
    .unwrap()
    .get(0);
    assert_eq!(reason, "government_official");
    let role: String = sqlx::query("SELECT role FROM person WHERE id = $1")
        .bind(person)
        .fetch_one(pg.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(role, "client");
    let participation: i64 =
        sqlx::query("SELECT count(*) FROM person_project_role WHERE person_id = $1")
            .bind(person)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
    assert_eq!(participation, 0);
    let edges: i64 = sqlx::query("SELECT count(*) FROM relationship")
        .fetch_one(pg.pool())
        .await
        .unwrap()
        .get(0);
    assert_eq!(edges, 0);
}
