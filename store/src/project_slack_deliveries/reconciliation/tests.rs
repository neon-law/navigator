use super::*;

async fn fixture(pg: &PostgresDb) -> Request {
    let (actor, notation, _) = crate::postgres::fixtures::workflow_fixture(pg).await;
    let (firm, project) = crate::postgres::fixtures::firm_for_notation(pg, notation).await;
    let brand = format!("synthetic-{}", Uuid::now_v7());
    sqlx::query("INSERT INTO brand(name,brand_key,firm_id) VALUES($1,$1,$2)")
        .bind(&brand)
        .bind(firm)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO firm_brand(firm_id,brand_key) VALUES($1,$2)")
        .bind(firm)
        .bind(&brand)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE project SET brand=$2 WHERE id=$1")
        .bind(project)
        .bind(&brand)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE person SET role='admin' WHERE id=$1")
        .bind(actor)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO person_firm_role(person_id,firm_id,membership) VALUES($1,$2,'admin')")
        .bind(actor)
        .bind(firm)
        .execute(pg.pool())
        .await
        .unwrap();
    let epoch: Uuid =
        sqlx::query_scalar("SELECT ownership_epoch FROM durable_object_ownership WHERE singleton")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    sqlx::query("INSERT INTO project_slack_handover(project_id,ownership_epoch,native_active,client_view_disposition) VALUES($1,$2,true,'unresolved')")
        .bind(project).bind(epoch).execute(pg.pool()).await.unwrap();
    Request {
        operation_id: Uuid::now_v7(),
        project_id: project,
        ownership_epoch: epoch,
        actor_person_id: actor,
        evidence_reference: Uuid::now_v7(),
        decision: Decision::Posted,
    }
}

async fn counts(pg: &PostgresDb, project: Uuid) -> (i64, i64, String) {
    let row: (i64,i64,String) = sqlx::query_as("SELECT (SELECT count(*) FROM project_slack_delivery_reconciliation WHERE project_id=$1),(SELECT count(*) FROM project_slack_delivery WHERE project_id=$1),client_view_disposition FROM project_slack_handover WHERE project_id=$1")
        .bind(project).fetch_one(pg.pool()).await.unwrap();
    row
}

#[tokio::test]
async fn posted_reconciliation_is_immutable_attributed_and_exactly_replayable() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let request = fixture(&pg).await;
        assert!(matches!(super::super::admission(&pg,request.project_id,"client_view","client-view").await,Err(super::super::DeliveryError::Unresolved)));
        reconcile(&pg, request).await.unwrap();
        reconcile(&pg, request).await.unwrap();
        assert_eq!(counts(&pg,request.project_id).await,(1,1,"posted".into()));
        assert_eq!(super::super::admission(&pg,request.project_id,"client_view","client-view").await.unwrap(),super::super::Admission::Posted);
        let audit: (Uuid,Uuid,String) = sqlx::query_as("SELECT actor_person_id,evidence_reference,decision FROM project_slack_delivery_reconciliation WHERE operation_id=$1")
            .bind(request.operation_id).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(audit,(request.actor_person_id,request.evidence_reference,"posted".into()));
        for sql in ["UPDATE project_slack_delivery_reconciliation SET decision='verified_not_posted' WHERE operation_id=$1", "DELETE FROM project_slack_delivery_reconciliation WHERE operation_id=$1"] {
            assert!(sqlx::query(sql).bind(request.operation_id).execute(pg.pool()).await.is_err());
        }
        let mut changed=request; changed.decision=Decision::VerifiedNotPosted;
        assert!(matches!(reconcile(&pg,changed).await,Err(ReconciliationError::Conflict)));
        changed=request;changed.evidence_reference=Uuid::now_v7();
        assert!(matches!(reconcile(&pg,changed).await,Err(ReconciliationError::Conflict)));
        assert_eq!(counts(&pg,request.project_id).await,(1,1,"posted".into()));
    }).await;
}

#[tokio::test]
async fn verified_absence_admits_future_delivery_without_fabricating_a_post() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let mut request = fixture(&pg).await;
        request.decision = Decision::VerifiedNotPosted;
        reconcile(&pg, request).await.unwrap();
        assert_eq!(
            counts(&pg, request.project_id).await,
            (1, 0, "not_posted".into())
        );
        assert_eq!(
            super::super::admission(&pg, request.project_id, "client_view", "client-view")
                .await
                .unwrap(),
            super::super::Admission::Allowed
        );
        super::super::record_posted(&pg, request.project_id, "client_view", "client-view")
            .await
            .unwrap();
        reconcile(&pg, request).await.unwrap();
        assert_eq!(
            counts(&pg, request.project_id).await,
            (1, 1, "not_posted".into())
        );
        let mut replacement = request;
        replacement.operation_id = Uuid::now_v7();
        replacement.decision = Decision::Posted;
        assert!(matches!(
            reconcile(&pg, replacement).await,
            Err(ReconciliationError::Conflict)
        ));
    })
    .await;
}

#[tokio::test]
async fn reconciliation_refuses_unadmitted_or_wrong_firm_actors_and_inactive_epochs() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let request = fixture(&pg).await;
        let foreign = fixture(&pg).await;
        let mut wrong_firm = request;
        wrong_firm.actor_person_id = foreign.actor_person_id;
        assert!(matches!(
            reconcile(&pg, wrong_firm).await,
            Err(ReconciliationError::Unauthorized)
        ));
        let mut invalid = request;
        invalid.evidence_reference = Uuid::nil();
        assert!(matches!(
            reconcile(&pg, invalid).await,
            Err(ReconciliationError::Invalid)
        ));
        for role in ["client", "clerk", "lawyer"] {
            sqlx::query("UPDATE person SET role=$2 WHERE id=$1")
                .bind(request.actor_person_id)
                .bind(role)
                .execute(pg.pool())
                .await
                .unwrap();
            assert!(matches!(
                reconcile(&pg, request).await,
                Err(ReconciliationError::Unauthorized)
            ));
        }
        sqlx::query("UPDATE person SET role='admin',is_admitted=false WHERE id=$1")
            .bind(request.actor_person_id)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(matches!(
            reconcile(&pg, request).await,
            Err(ReconciliationError::Unauthorized)
        ));
        sqlx::query("UPDATE person SET is_admitted=true WHERE id=$1")
            .bind(request.actor_person_id)
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query("DELETE FROM person_firm_role WHERE person_id=$1")
            .bind(request.actor_person_id)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(matches!(
            reconcile(&pg, request).await,
            Err(ReconciliationError::Unauthorized)
        ));
        let mut stale = request;
        stale.ownership_epoch = Uuid::now_v7();
        assert!(matches!(
            reconcile(&pg, stale).await,
            Err(ReconciliationError::Inactive)
        ));
        sqlx::query("UPDATE project_slack_handover SET native_active=false,capture_id=(SELECT capture_id FROM durable_object_ownership WHERE singleton) WHERE project_id=$1")
            .bind(request.project_id)
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE person SET role='owner' WHERE id=$1")
            .bind(request.actor_person_id)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(matches!(
            reconcile(&pg, request).await,
            Err(ReconciliationError::Inactive)
        ));
        sqlx::query("UPDATE project_slack_handover SET native_active=true WHERE project_id=$1")
            .bind(request.project_id)
            .execute(pg.pool())
            .await
            .unwrap();
        reconcile(&pg, request).await.unwrap();
        assert_eq!(
            counts(&pg, request.project_id).await,
            (1, 1, "posted".into())
        );
    })
    .await;
}

#[tokio::test]
async fn competing_decisions_serialize_on_the_native_project_lane() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let first = fixture(&pg).await;
        let mut second = first;
        second.operation_id = Uuid::now_v7();
        second.decision = Decision::VerifiedNotPosted;
        let mut blocker = pg.pool().begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(super::super::lane_key(first.project_id))
            .execute(&mut *blocker)
            .await
            .unwrap();
        let mut future = Box::pin(reconcile(&pg, first));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut future)
                .await
                .is_err()
        );
        assert_eq!(
            counts(&pg, first.project_id).await,
            (0, 0, "unresolved".into())
        );
        blocker.commit().await.unwrap();
        future.await.unwrap();
        assert!(matches!(
            reconcile(&pg, second).await,
            Err(ReconciliationError::Conflict)
        ));
        assert_eq!(counts(&pg, first.project_id).await, (1, 1, "posted".into()));
    })
    .await;
}

#[tokio::test]
async fn a_failed_posted_receipt_rolls_back_disposition_and_immutable_audit() {
    crate::test_support::with_postgres(async {
        let pg=PostgresDb::application().await.unwrap();let request=fixture(&pg).await;
        sqlx::raw_sql("CREATE FUNCTION reject_test_slack_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic receipt failure'; END; $$; CREATE TRIGGER reject_test_slack_receipt BEFORE INSERT ON project_slack_delivery FOR EACH ROW EXECUTE FUNCTION reject_test_slack_receipt();")
            .execute(pg.pool()).await.unwrap();
        assert!(matches!(reconcile(&pg,request).await,Err(ReconciliationError::Storage)));
        assert_eq!(counts(&pg,request.project_id).await,(0,0,"unresolved".into()));
        sqlx::raw_sql("DROP TRIGGER reject_test_slack_receipt ON project_slack_delivery; DROP FUNCTION reject_test_slack_receipt();")
            .execute(pg.pool()).await.unwrap();
        reconcile(&pg,request).await.unwrap();
        assert_eq!(counts(&pg,request.project_id).await,(1,1,"posted".into()));
    }).await;
}

#[tokio::test]
async fn canonical_project_deletion_preserves_audit_without_bypassing_protected_records() {
    crate::test_support::with_postgres(async {
        let pg=PostgresDb::application().await.unwrap();let request=fixture(&pg).await;
        reconcile(&pg,request).await.unwrap();
        let source=crate::application_store::ApplicationStore::uninitialized();
        let protected = crate::projects::delete_project(&source, request.project_id).await;
        assert!(matches!(protected, Err(crate::projects::ProjectCommandError::Referenced(_))), "protected Project deletion returned {protected:?}");
        assert!(crate::projects::find_by_id(&source,request.project_id).await.unwrap().is_some());
        // Remove only this fixture's protected document to exercise the empty case.
        sqlx::query("DELETE FROM notation WHERE project_id=$1").bind(request.project_id).execute(pg.pool()).await.unwrap();
        crate::projects::delete_project(&source,request.project_id).await.unwrap();
        assert!(crate::projects::find_by_id(&source,request.project_id).await.unwrap().is_none());
        let retained: (Uuid,Uuid,Uuid,String)=sqlx::query_as("SELECT project_id,actor_person_id,evidence_reference,decision FROM project_slack_delivery_reconciliation WHERE operation_id=$1")
            .bind(request.operation_id).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(retained,(request.project_id,request.actor_person_id,request.evidence_reference,"posted".into()));
        let receipts:i64=sqlx::query_scalar("SELECT count(*) FROM project_slack_delivery WHERE project_id=$1")
            .bind(request.project_id).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(receipts,0);
    }).await;
}

#[tokio::test]
async fn firm_authorization_reuses_the_only_available_transaction_connection() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let request = fixture(&pg).await;
        let maximum = pg.pool().options().get_max_connections();
        let mut held = Vec::new();
        for _ in 1..maximum {
            held.push(pg.pool().acquire().await.unwrap());
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), reconcile(&pg, request))
            .await
            .expect("reconciliation must not acquire a second pool connection")
            .unwrap();
        drop(held);
        assert_eq!(
            counts(&pg, request.project_id).await,
            (1, 1, "posted".into())
        );
    })
    .await;
}
