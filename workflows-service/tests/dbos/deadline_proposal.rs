//! A lawyer can confirm while the executor is down. A fresh host drains the approved revision once.
use crate::support::{database, native_project, APP_VERSION, DEADLINE};
use store::deadline_proposals::{self as queue, Decision, Draft, Status};
use uuid::Uuid;
use workflows_service::dbos_deadline_proposal as publisher;

async fn published_deadlines(pg: &store::postgres::PostgresDb, project: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM project_deadlines WHERE project_id=$1")
        .bind(project)
        .fetch_one(pg.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn approved_while_offline_is_published_once_after_worker_restart() {
    let Some(fixture) = database().await else {
        return;
    };
    let pg = fixture.store();
    let (matter, lawyer) = store::postgres::PostgresDb::with_test_db(
        pg.clone(),
        native_project::matter(&store::application_store::ApplicationStore::uninitialized()),
    )
    .await;
    sqlx::query("INSERT INTO person_project_role(project_id,person_id,participation) VALUES($1,$2,'lawyer') ON CONFLICT DO NOTHING").bind(matter.id).bind(lawyer).execute(pg.pool()).await.unwrap();
    let doc = Uuid::now_v7();
    let proposal = Uuid::new_v4();
    let hash = "b".repeat(64);
    sqlx::query("INSERT INTO asset(id,project_id,kind,storage_key,content_type,byte_size,sha256_hex) VALUES($1,$2,'order','fixture','application/pdf',12,$3)").bind(doc).bind(matter.id).bind(&hash).execute(pg.pool()).await.unwrap();
    let draft:Draft=serde_json::from_value(serde_json::json!({"kind":"signed_order","title":"Synthetic response","timezone":"America/New_York","due_on":"2028-04-05","evidence":[{"role":"signed_order","id":doc,"sha256_hex":hash,"page":1,"rects":[{"x":0.1,"y":0.1,"width":0.5,"height":0.1}]}]})).unwrap();
    let dbos = dbos::DBOS::new(workflows::dbos_engine::config(&fixture.url, APP_VERSION));
    let worker = publisher::register(&dbos, pg.clone()).unwrap();
    dbos.launch().await.unwrap();
    queue::stage(&pg, matter.id, proposal, None, &draft)
        .await
        .unwrap();
    let job = queue::work(&pg).await.unwrap()[0];
    let id = publisher::workflow_id(job);
    let first_stop = dbos::CancellationToken::new();
    let first = tokio::spawn(publisher::schedule(
        dbos.clone(),
        pg.clone(),
        worker.clone(),
        first_stop.clone(),
    ));
    tokio::time::timeout(DEADLINE, async {
        loop {
            if dbos
                .list_workflow_steps(&id)
                .await
                .is_ok_and(|steps| !steps.is_empty())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("workflow checkpoints pending human review");
    assert_eq!(
        queue::get(&pg, matter.id, proposal)
            .await
            .unwrap()
            .unwrap()
            .status,
        Status::Pending
    );
    assert_eq!(published_deadlines(&pg, matter.id).await, 0);
    first_stop.cancel();
    first.await.unwrap();
    dbos.shutdown().await;
    queue::decide(&pg, matter.id, proposal, lawyer, 1, Decision::Confirm)
        .await
        .unwrap();
    dbos.launch().await.unwrap();
    let cancel = dbos::CancellationToken::new();
    let task = tokio::spawn(publisher::schedule(
        dbos.clone(),
        pg.clone(),
        worker,
        cancel.clone(),
    ));
    tokio::time::timeout(DEADLINE, async {
        loop {
            if queue::get(&pg, matter.id, proposal)
                .await
                .unwrap()
                .unwrap()
                .status
                == Status::Published
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the restarted worker drains the approval");
    cancel.cancel();
    task.await.unwrap();
    dbos.shutdown().await;
    assert_eq!(published_deadlines(&pg, matter.id).await, 1);
    drop(pg);
    fixture.close().await;
}
