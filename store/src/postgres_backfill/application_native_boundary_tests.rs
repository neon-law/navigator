//! Captured nonempty Application rows survive handoff and native-only reads.

use super::*;
use crate::postgres_mirror::tests::verified_family_capture;

const TABLES: [&str; 9] = [
    "mailroom",
    "letter",
    "lead",
    "lead_consent",
    "project_deadlines",
    "project_service",
    "summary_sender_approval",
    "summary_sender_approval_event",
    "inbox_triage",
];

struct Rows {
    mailroom: Uuid,
    letter: Uuid,
    lead: Uuid,
    consent: Uuid,
    deadline: Uuid,
    service: Uuid,
    approval: Uuid,
    approval_event: Uuid,
    triage: Uuid,
    message: Uuid,
}

async fn prior_histories(pg: &PostgresDb) -> serde_json::Value {
    sqlx::query_scalar("SELECT jsonb_build_object('runs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY r.scope,r.id) FROM mirror_backfill_run r WHERE r.scope <> 'application:1'),'checkpoints',(SELECT jsonb_agg(to_jsonb(c) ORDER BY c.run_id,c.mirror) FROM mirror_backfill_checkpoint c JOIN mirror_backfill_run r ON r.id=c.run_id WHERE r.scope <> 'application:1'))")
        .fetch_one(pg.pool()).await.unwrap()
}

async fn captured_rows(
    pg: &PostgresDb,
    source: &SurrealDb,
    actor: Uuid,
    project: Uuid,
    firm: Uuid,
) -> Rows {
    let rows = Rows {
        mailroom: Uuid::now_v7(),
        letter: Uuid::now_v7(),
        lead: Uuid::now_v7(),
        consent: Uuid::now_v7(),
        deadline: Uuid::now_v7(),
        service: Uuid::now_v7(),
        approval: Uuid::now_v7(),
        approval_event: Uuid::now_v7(),
        triage: Uuid::now_v7(),
        message: Uuid::now_v7(),
    };
    let address: Uuid = sqlx::query_scalar("SELECT id FROM address ORDER BY id LIMIT 1")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    // These parents were genuinely transferred by the prerequisite fixture.
    let document: Uuid =
        sqlx::query_scalar("SELECT id FROM asset WHERE project_id=$1 ORDER BY id LIMIT 1")
            .bind(project)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    let message: Uuid = sqlx::query_scalar("SELECT m.id FROM message m JOIN inbox_conversation c ON c.id=m.conversation_id WHERE c.project_id=$1 ORDER BY m.id LIMIT 1")
        .bind(project).fetch_one(pg.pool()).await.unwrap();
    let rows = Rows { message, ..rows };
    source.query("CREATE $mailroom SET name='Synthetic captured mailroom',address_id=$address; CREATE $letter SET mailroom_id=$mailroom,project_id=$project,direction='incoming',sender='Synthetic sender',recipient='Synthetic recipient',summary='Synthetic captured letter'; CREATE $lead SET email='boundary@example.invalid',brand_key='synthetic-tenancy-capture',source_path='/synthetic',consent_version='v1',consented_at=$at,person_id=$actor; CREATE $consent SET lead_id=$lead,consented_at=$at,source_path='/synthetic',sms_consent_version='sms-v1',sms_policy_version='policy-v1',sms_opt_in=true,call_consent_version='call-v1',call_opt_in=false; CREATE $deadline SET project_id=$project,title='Synthetic deadline',due_at='2026-12-01T12:00:00Z',authority_kind='internal',authority='Synthetic authority',source='Synthetic capture',owner_person_id=$actor,status='open',calendar_role='firm',category='internal',inserted_at='2026-10-15T12:00:00.123456789Z',updated_at='2026-10-15T12:00:00.987654321Z'; CREATE $service SET project_id=$project,document_id=$document,recipient='Synthetic recipient',recipient_person_id=$actor,method='e_service',served_on='2026-10-15',address='boundary@example.invalid',declarant='Synthetic declarant',declarant_person_id=$actor,letter_id=$letter,inserted_at='2026-10-15T12:00:00.123456789Z',updated_at='2026-10-15T12:00:00.987654321Z'; CREATE $approval SET firm_id=$firm,project_id=$project,provider_account='synthetic',person_id=$actor,sender_email='boundary@example.invalid',status='approved',updated_at=$at; CREATE $approval_event SET firm_id=$firm,project_id=$project,provider_account='synthetic',person_id=$actor,actor_person_id=$actor,sender_email='boundary@example.invalid',action='approved',inserted_at=$at; CREATE $triage SET message_id=$message,firm_id=$firm,suggested_project_id=$project,project_confidence=0.75,privilege='uncertain',rationale='Synthetic review required',prompt_version='synthetic-v1',disposition='suggested',labeled_example=true,inserted_at=$at,updated_at=$at")
        .bind(("document", crate::surreal::record_id("asset", document)))
        .bind(("message", crate::surreal::record_id("message", rows.message)))
        .bind(("project", crate::surreal::record_id("project", project)))
        .bind(("firm", crate::surreal::record_id("firm", firm)))
        .bind(("actor", crate::surreal::record_id("person", actor)))
        .bind(("address", crate::surreal::record_id("address", address)))
        .bind(("mailroom", crate::surreal::record_id("mailroom", rows.mailroom)))
        .bind(("letter", crate::surreal::record_id("letter", rows.letter)))
        .bind(("lead", crate::surreal::record_id("lead", rows.lead)))
        .bind(("consent", crate::surreal::record_id("lead_consent", rows.consent)))
        .bind(("deadline", crate::surreal::record_id("project_deadlines", rows.deadline)))
        .bind(("service", crate::surreal::record_id("project_service", rows.service)))
        .bind(("approval", crate::surreal::record_id("summary_sender_approval", rows.approval)))
        .bind(("approval_event", crate::surreal::record_id("summary_sender_approval_event", rows.approval_event)))
        .bind(("triage", crate::surreal::record_id("inbox_triage", rows.triage)))
        .bind(("at", surrealdb::types::Datetime::from("2026-10-15T12:00:00.123456789Z".parse::<chrono::DateTime<chrono::Utc>>().unwrap())))
        .await.unwrap().check().unwrap();
    rows
}

#[tokio::test]
async fn captured_application_boundaries_activate_and_read_without_source() {
    let Some((pg, source, actor, project, firm)) =
        Box::pin(super::application_family_tests::database_with_domain_parents()).await
    else {
        return;
    };
    let before = prior_histories(&pg).await;
    let rows = Box::pin(captured_rows(&pg, &source, actor, project, firm)).await;
    let receipt = Box::pin(verified_family_capture(&source)).await;
    let started = Box::pin(start_family(
        &source,
        &pg,
        family::Family::Application,
        receipt,
        1,
        Some(1),
        false,
    ))
    .await
    .unwrap();
    assert_eq!(started.status, RunStatus::Paused);
    let copied = Box::pin(resume_family(
        &source,
        &pg,
        family::Family::Application,
        started.run_id,
        None,
        false,
    ))
    .await
    .unwrap();
    assert!(copied.failures.is_empty(), "{copied:?}");
    let completed = Box::pin(resume_family(
        &source,
        &pg,
        family::Family::Application,
        started.run_id,
        None,
        true,
    ))
    .await
    .unwrap();
    assert!(completed.is_clean(), "{completed:?}");
    for table in TABLES {
        let parity = completed
            .parity
            .iter()
            .find(|row| row.table == table)
            .unwrap();
        assert_eq!((parity.surreal, parity.postgres), (1, 1), "{table}");
    }
    assert_eq!(prior_histories(&pg).await, before);
    Box::pin(native_reads(&pg, &rows, actor, project, firm)).await;
    let replay = Box::pin(resume_family(
        &SurrealDb::uninitialized(),
        &pg,
        family::Family::Application,
        started.run_id,
        None,
        true,
    ))
    .await
    .unwrap();
    assert!(replay.is_clean(), "{replay:?}");
    assert_eq!(prior_histories(&pg).await, before);
    pg.pool().close().await;
}

async fn native_reads(pg: &PostgresDb, rows: &Rows, actor: Uuid, project: Uuid, firm: Uuid) {
    PostgresDb::with_test_db(pg.clone(), async {
        PostgresDb::application().await.unwrap();
        let offline = SurrealDb::uninitialized();
        assert_eq!(
            crate::mailrooms::find_by_id(&offline, rows.mailroom)
                .await
                .unwrap()
                .unwrap()
                .name,
            "Synthetic captured mailroom"
        );
        assert_eq!(
            crate::letters::find_by_id(&offline, rows.letter)
                .await
                .unwrap()
                .unwrap()
                .summary,
            "Synthetic captured letter"
        );
        assert_eq!(
            crate::leads::find(&offline, rows.lead)
                .await
                .unwrap()
                .unwrap()
                .email,
            "boundary@example.invalid"
        );
        assert_eq!(
            crate::leads::consent_history(&offline, rows.lead)
                .await
                .unwrap()[0]
                .id,
            rows.consent
        );
        let deadlines = crate::project_deadlines::list(&offline, project)
            .await
            .unwrap();
        assert_eq!(deadlines[0].id, rows.deadline);
        assert_eq!(
            deadlines[0]
                .due_at
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap(),
            "2026-12-01T12:00:00Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap()
        );
        assert_eq!(
            crate::project_service::find(&offline, project, rows.service)
                .await
                .unwrap()
                .unwrap()
                .letter_id,
            Some(rows.letter)
        );
        assert_eq!(
            crate::summary_sender_approvals::list_for_firm(
                &offline,
                crate::persons::Role::Owner,
                Some(actor),
                firm
            )
            .await
            .unwrap()[0]
                .id,
            rows.approval
        );
        assert_eq!(
            crate::summary_sender_approvals::audit_history_for_firm(
                &offline,
                crate::persons::Role::Owner,
                Some(actor),
                firm
            )
            .await
            .unwrap()[0]
                .id,
            rows.approval_event
        );
        assert_eq!(
            crate::inbox_triage::find_for_message(&offline, rows.message)
                .await
                .unwrap()
                .unwrap()
                .id,
            rows.triage
        );
    })
    .await;
}

#[tokio::test]
async fn activated_postgres_seeds_real_local_portfolio_without_source() {
    let Some((pg, source, _, _, _)) =
        Box::pin(super::application_family_tests::database_with_domain_parents()).await
    else {
        return;
    };
    let receipt = Box::pin(verified_family_capture(&source)).await;
    let completed = Box::pin(start_family(
        &source,
        &pg,
        family::Family::Application,
        receipt,
        10,
        None,
        true,
    ))
    .await
    .unwrap();
    assert!(completed.is_clean(), "{completed:?}");
    let before = prior_histories(&pg).await;
    crate::clauses::seed(&pg, &rules::clauses::library_clauses().unwrap())
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let storage: std::sync::Arc<dyn cloud::StorageService> =
        std::sync::Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
    PostgresDb::with_test_db(pg.clone(), async {
        PostgresDb::application().await.unwrap();
        let offline = SurrealDb::uninitialized();
        let report = Box::pin(crate::seed::seed_environment(
            &offline,
            &storage,
            crate::DeploymentEnvironment::Dev,
            crate::seed::BrandSeed::Neon,
        ))
        .await
        .unwrap();
        assert!(report.projects_inserted >= 3, "{report:?}");
        let seeded: i64 = sqlx::query_scalar("SELECT count(*) FROM project WHERE code=ANY($1)")
            .bind(["sample-litigation", "sample-transactional", "sample-estate"])
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(seeded, 3);
        assert!(crate::mailrooms::count(&offline).await.unwrap() > 0);
        assert!(!storage.list("").await.unwrap().is_empty());
    })
    .await;
    assert_eq!(prior_histories(&pg).await, before);
    pg.pool().close().await;
}
