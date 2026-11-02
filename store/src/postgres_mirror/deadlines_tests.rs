use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::test_support::mem_surreal;

#[test]
fn deadline_mirror_key_is_opaque() {
    let id = Uuid::nil();
    let key = MirrorKey::ProjectDeadline(id);
    assert_eq!(key.table(), "project_deadlines");
    assert_eq!(key.to_string(), format!("project_deadlines:{id}"));
}

/// Assert the mirrored row replayed with every tracked column intact.
async fn assert_replayed_row(pg: &PostgresDb, id: Uuid) {
    let (status, title, description, sequence, category): (
        String,
        Option<String>,
        Option<String>,
        i64,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT status, title, description, calendar_sequence, category \
         FROM project_deadlines WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(status, "complete");
    assert_eq!(title.as_deref(), Some("Answer"));
    assert_eq!(description.as_deref(), Some("Synthetic instructions"));
    assert_eq!(sequence, 7);
    assert_eq!(category.as_deref(), Some("discovery"));
}

#[tokio::test]
async fn project_deadline_mirrors_replay_and_reconcile() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let jurisdiction = Uuid::now_v7();
    let entity_type = Uuid::now_v7();
    let entity = Uuid::now_v7();
    let project = Uuid::now_v7();
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
    sqlx::query("INSERT INTO project (id,code,name,status,entity_id) VALUES ($1,$2,'Synthetic project','open',$3)")
        .bind(project).bind(format!("deadline-{project}")).bind(entity).execute(&pg.0).await.unwrap();

    surreal.query("CREATE $id SET project_id=$project, calendar_sequence=7, title='Answer', description='Synthetic instructions', due_at='2027-04-05T12:00:00Z', authority_kind='internal', authority='', source='synthetic test', status='open', calendar_role='firm', category='discovery', inserted_at='2026-10-15T12:00:00Z', updated_at='2026-10-15T12:00:00Z'")
        .bind(("id", crate::surreal::record_id("project_deadlines", id)))
        .bind(("project", crate::surreal::record_id("project", project)))
        .await.unwrap().check().unwrap();

    let key = MirrorKey::ProjectDeadline(id);
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Unchanged
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());

    // A row whose category is cleared (the historical shape) mirrors back and
    // settles at parity with no category on either side.
    surreal
        .query("UPDATE $id UNSET category")
        .bind(("id", crate::surreal::record_id("project_deadlines", id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        mirror(&surreal, &pg, &key).await.unwrap(),
        Mirrored::Written
    );
    assert!(parity(&surreal, &pg).await.unwrap().is_clean());

    // Restore the categorized shape so the reconcile leg below proves a
    // category survives a full replay into a deleted-and-rebuilt row.
    surreal
        .query("UPDATE $id SET category='discovery'")
        .bind(("id", crate::surreal::record_id("project_deadlines", id)))
        .await
        .unwrap()
        .check()
        .unwrap();

    surreal
        .query("UPDATE $id SET status='complete', updated_at='2026-10-16T12:00:00Z'")
        .bind(("id", crate::surreal::record_id("project_deadlines", id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    sqlx::query("DELETE FROM project_deadlines WHERE id=$1")
        .bind(id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(reconcile(&surreal, &pg).await.unwrap().is_clean());
    assert_replayed_row(&pg, id).await;
    pg.0.close().await;
}
