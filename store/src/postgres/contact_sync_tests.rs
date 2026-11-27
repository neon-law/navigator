use cloud::contacts::FakeContactBooks;
use uuid::Uuid;

use super::{participation, projects};
use crate::contact_sync;
use crate::postgres::fixtures::{connect, fresh_schema};
use crate::projects::NewProject;

#[tokio::test]
async fn contact_sync_uses_open_project_participants_and_removes_closed_people() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let ids = [
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    ];
    let [entity_type, jurisdiction, entity, firm, lawyer, client] = ids;
    sqlx::query(
        "INSERT INTO jurisdiction(id,name,code) VALUES ($1,'Synthetic Jurisdiction','TST')",
    )
    .bind(jurisdiction)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO entity_type(id,name) VALUES ($1,'Synthetic Entity Type')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)")
        .bind(entity).bind(entity_type).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO firm(id,name,slug,status,entity_id) VALUES ($1,'Synthetic Firm','synthetic-firm','active',$2)")
        .bind(firm).bind(entity).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO brand(name,brand_key) VALUES ('Synthetic Brand','neon') ON CONFLICT (brand_key) DO NOTHING")
        .execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO person(id,name,email,role) VALUES ($1,'Synthetic Lawyer','lawyer@example.test','lawyer'),($2,'Synthetic Client','client@example.test','client')")
        .bind(lawyer).bind(client).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO person_mailbox(id,person_id) VALUES ('client.alt@example.test',$1)")
        .bind(client)
        .execute(pg.pool())
        .await
        .unwrap();
    let project = projects::create_on(
        &pg,
        &NewProject {
            code: "synthetic-project".into(),
            name: "Synthetic Project".into(),
            status: "open".into(),
            brand: "neon".into(),
            entity_id: entity,
            firm_id: Some(firm),
            jurisdiction_id: Some(jurisdiction),
            ..NewProject::default()
        },
    )
    .await
    .unwrap();
    participation::add_on(&pg, project.id, lawyer)
        .await
        .unwrap();
    participation::add_on(&pg, project.id, client)
        .await
        .unwrap();
    let mut tx = pg.pool().begin().await.unwrap();
    sqlx::query("INSERT INTO person_phone(id,person_id,e164,label,is_primary,inserted_at,updated_at) VALUES ($1,$2,'+15555550100','mobile',true,now(),now())")
        .bind(Uuid::new_v4()).bind(client).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();

    let books = FakeContactBooks::default();
    let report = contact_sync::sync_project_members_with_postgres(&pg, project.id, &books)
        .await
        .unwrap();
    assert_eq!(report.created, 1);
    assert_eq!(
        books.contacts("lawyer@example.test")[&client.to_string()].phones[0].value,
        "+15555550100"
    );
    assert_eq!(
        books.contacts("lawyer@example.test")[&client.to_string()].emails,
        ["client.alt@example.test", "client@example.test"]
    );
    assert!(books.contacts("client@example.test").is_empty());

    sqlx::query("UPDATE project SET status='closed' WHERE id=$1")
        .bind(project.id)
        .execute(pg.pool())
        .await
        .unwrap();
    let report = contact_sync::sync_project_members_with_postgres(&pg, project.id, &books)
        .await
        .unwrap();
    assert_eq!(report.removed, 1);
    assert!(books.contacts("lawyer@example.test").is_empty());
}
