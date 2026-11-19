//! Native Application rows remain owned even under forced legacy replay modes.

use super::*;

#[tokio::test]
async fn active_application_replay_preserves_native_rows_without_source_access() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let offline = SurrealDb::uninitialized();
        let address = crate::addresses::create(
            &offline,
            &crate::addresses::NewAddress {
                line1: "100 Synthetic Way".into(),
                city: "Reno".into(),
                region: "NV".into(),
                postal_code: "89501".into(),
                country: "US".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mailroom = crate::mailrooms::create(&offline, "Synthetic native mailroom", address.id)
            .await
            .unwrap();
        let letter = crate::letters::record(
            &offline,
            &crate::letters::NewLetter {
                mailroom_id: mailroom.id,
                project_id: None,
                direction: "incoming".into(),
                sender: "Synthetic sender".into(),
                recipient: "Synthetic recipient".into(),
                summary: "Native history".into(),
            },
        )
        .await
        .unwrap();
        let lead = crate::leads::record_consent_event(
            &offline,
            &crate::leads::NewLead {
                email: "replay@example.invalid".into(),
                phone: None,
                brand_key: "synthetic".into(),
                source_path: "/synthetic".into(),
                consent_version: "v1".into(),
                consented_at: chrono::Utc::now(),
            },
            &crate::leads::NewConsentEvent {
                consented_at: chrono::Utc::now(),
                phone: None,
                source_path: "/synthetic".into(),
                sms_consent_version: "sms-v1".into(),
                sms_policy_version: "policy-v1".into(),
                sms_opt_in: true,
                call_consent_version: "call-v1".into(),
                call_opt_in: false,
            },
        )
        .await
        .unwrap();
        let service = native_service(&offline, &pg, letter.id).await;
        let before = snapshot(&pg).await;
        check_replay(&offline, &pg, mailroom.id, letter.id, lead.id, service).await;
        assert_eq!(snapshot(&pg).await, before);
        sqlx::query("ALTER TABLE mirror_backfill_checkpoint RENAME TO hidden_checkpoint")
            .execute(pg.pool())
            .await
            .unwrap();
        check_ledger_errors(&offline, &pg, letter.id, lead.id, service).await;
        sqlx::query("ALTER TABLE hidden_checkpoint RENAME TO mirror_backfill_checkpoint")
            .execute(pg.pool())
            .await
            .unwrap();
        assert_eq!(snapshot(&pg).await, before);
    })
    .await;
}

async fn snapshot(pg: &PostgresDb) -> serde_json::Value {
    sqlx::query_scalar("SELECT jsonb_build_object('mailrooms',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM mailroom r),'letters',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM letter r),'leads',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM lead r),'consents',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM lead_consent r),'services',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM project_service r),'approvals',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM summary_sender_approval r),'approval_events',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM summary_sender_approval_event r),'deadlines',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM project_deadlines r),'runs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM mirror_backfill_run r),'checkpoints',(SELECT jsonb_agg(to_jsonb(r) ORDER BY run_id,mirror) FROM mirror_backfill_checkpoint r))")
        .fetch_one(pg.pool()).await.unwrap()
}

async fn native_project(offline: &SurrealDb) -> (crate::projects::Project, Uuid, Uuid) {
    let jurisdiction = crate::jurisdictions::create(
        offline,
        &crate::jurisdictions::NewJurisdiction::new("Synthetic native jurisdiction", "SN", "state"),
    )
    .await
    .unwrap();
    let entity_type = crate::entity_types::create(offline, "Synthetic native entity type")
        .await
        .unwrap();
    let entity = crate::entities::create(
        offline,
        &crate::entities::NewEntity {
            name: "Synthetic native entity".into(),
            entity_type_id: entity_type.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap();
    let person = crate::persons::create(
        offline,
        &crate::persons::NewPerson::with_role(
            "Synthetic native owner",
            "owner@example.invalid",
            crate::persons::Role::Owner,
        ),
    )
    .await
    .unwrap();
    let firm = crate::firms::create(
        offline,
        &crate::firms::NewFirm {
            name: "Synthetic native firm".into(),
            status: "active".into(),
            entity_id: entity.id,
            admin_dri_person_id: person.id,
        },
    )
    .await
    .unwrap();
    let brand = crate::brands::create(
        offline,
        crate::persons::Role::Owner,
        Some(person.id),
        &crate::brands::NewBrand {
            name: "Synthetic native brand".into(),
            key: "synthetic-native".into(),
            firm_id: Some(firm.id),
            primary_color: None,
            accent_color: None,
            typeface: None,
        },
    )
    .await
    .unwrap();
    crate::firms::ensure_brand(offline, firm.id, &brand.key)
        .await
        .unwrap();
    let project = crate::projects::create(
        offline,
        &crate::projects::NewProject {
            code: "synthetic-native-service".into(),
            brand: brand.key,
            name: "Synthetic native service".into(),
            status: "open".into(),
            entity_id: entity.id,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (project, person.id, firm.id)
}

async fn native_service(offline: &SurrealDb, pg: &PostgresDb, letter: Uuid) -> Uuid {
    let (project, person, firm) = native_project(offline).await;
    native_decisions(offline, pg, project.id, person, firm).await;
    let dir = tempfile::tempdir().unwrap();
    let storage: std::sync::Arc<dyn cloud::StorageService> =
        std::sync::Arc::new(cloud::FsStorage::new(dir.path()).await.unwrap());
    let document = crate::documents::ingest_bytes(
        offline,
        &storage,
        &crate::documents::IngestArgs {
            project_id: project.id,
            source: "upload",
            filename: "synthetic.txt",
            kind: "unclassified",
            content_type: "text/plain",
            description: None,
            visibility: "internal",
            secondary_storage_key: None,
        },
        b"Synthetic retained document",
    )
    .await
    .unwrap();
    crate::project_service::create(
        offline,
        &crate::project_service::NewService {
            project_id: project.id,
            document_id: document.asset_id,
            recipient: "Synthetic recipient".into(),
            recipient_person_id: None,
            recipient_entity_id: None,
            counsel_for: None,
            method: "mail".into(),
            served_on: "2026-10-15".into(),
            address: "Synthetic address".into(),
            declarant: "Synthetic declarant".into(),
            declarant_person_id: None,
            letter_id: Some(letter),
            sent_email_id: None,
        },
    )
    .await
    .unwrap()
    .id
}

async fn check_replay(
    offline: &SurrealDb,
    pg: &PostgresDb,
    mailroom: Uuid,
    letter: Uuid,
    lead: Uuid,
    service: Uuid,
) {
    let consent: Uuid = sqlx::query_scalar("SELECT id FROM lead_consent WHERE lead_id=$1")
        .bind(lead)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    from_source(during_initial_copy(async {
        let foreign_key = MirrorKey::Project(Uuid::now_v7());
        assert!(matches!(
            mailrooms_letters::mirror(offline, pg, &foreign_key).await,
            Err(MirrorError::Source)
        ));
        assert!(matches!(
            leads::mirror(offline, pg, &foreign_key).await,
            Err(MirrorError::Source)
        ));
        assert!(matches!(
            communications::mirror(offline, pg, &foreign_key).await,
            Err(MirrorError::Source)
        ));
        for key in [MirrorKey::Mailroom(mailroom), MirrorKey::Letter(letter)] {
            assert_eq!(
                mailrooms_letters::mirror(offline, pg, &key).await.unwrap(),
                Mirrored::Unchanged
            );
        }
        assert!(mailrooms_letters::parity(offline, pg)
            .await
            .unwrap()
            .tables
            .is_empty());
        assert!(mailrooms_letters::reconcile(offline, pg)
            .await
            .unwrap()
            .tables
            .is_empty());
        for key in [MirrorKey::Lead(lead), MirrorKey::LeadConsent(consent)] {
            assert_eq!(
                leads::mirror(offline, pg, &key).await.unwrap(),
                Mirrored::Unchanged
            );
        }
        assert!(leads::parity(offline, pg).await.unwrap().tables.is_empty());
        assert!(leads::reconcile(offline, pg)
            .await
            .unwrap()
            .tables
            .is_empty());
        assert_eq!(
            project_services::mirror(offline, pg, &MirrorKey::ProjectService(service))
                .await
                .unwrap(),
            Mirrored::Unchanged
        );
        assert!(project_services::parity(offline, pg)
            .await
            .unwrap()
            .tables
            .is_empty());
        assert!(project_services::reconcile(offline, pg)
            .await
            .unwrap()
            .tables
            .is_empty());
        check_remaining_replay(offline, pg).await;
    }))
    .await;
}

async fn check_ledger_errors(
    offline: &SurrealDb,
    pg: &PostgresDb,
    letter: Uuid,
    lead: Uuid,
    service: Uuid,
) {
    from_source(during_initial_copy(async {
        assert!(
            mailrooms_letters::mirror(offline, pg, &MirrorKey::Letter(letter))
                .await
                .is_err()
        );
        assert!(mailrooms_letters::parity(offline, pg).await.is_err());
        assert!(mailrooms_letters::reconcile(offline, pg).await.is_err());
        assert!(leads::mirror(offline, pg, &MirrorKey::Lead(lead))
            .await
            .is_err());
        assert!(leads::parity(offline, pg).await.is_err());
        assert!(leads::reconcile(offline, pg).await.is_err());
        assert!(
            project_services::mirror(offline, pg, &MirrorKey::ProjectService(service))
                .await
                .is_err()
        );
        assert!(project_services::parity(offline, pg).await.is_err());
        assert!(project_services::reconcile(offline, pg).await.is_err());
        assert!(communications::mirror(
            offline,
            pg,
            &MirrorKey::SummarySenderApproval(Uuid::now_v7())
        )
        .await
        .is_err());
        assert!(communications::parity(offline, pg).await.is_err());
        assert!(communications::reconcile(offline, pg).await.is_err());
        assert!(
            deadlines::mirror(offline, pg, &MirrorKey::ProjectDeadline(Uuid::now_v7()))
                .await
                .is_err()
        );
        assert!(deadlines::parity(offline, pg).await.is_err());
        assert!(deadlines::reconcile(offline, pg).await.is_err());
    }))
    .await;
}

async fn native_decisions(
    offline: &SurrealDb,
    pg: &PostgresDb,
    project: Uuid,
    person: Uuid,
    firm: Uuid,
) {
    let client = crate::persons::create(
        offline,
        &crate::persons::NewPerson::with_role(
            "Synthetic native client",
            "client@example.invalid",
            crate::persons::Role::Client,
        ),
    )
    .await
    .unwrap();
    crate::persons::set_email_confirmed(offline, client.id, true)
        .await
        .unwrap();
    crate::projects::designate_dri_in_surreal(
        offline,
        project,
        client.id,
        crate::projects::DriSide::Client,
    )
    .await
    .unwrap();
    crate::postgres::summary_sender_approvals::approve(
        pg,
        person,
        firm,
        project,
        "synthetic",
        "client@example.invalid",
        client.id,
    )
    .await
    .unwrap();
    crate::project_deadlines::create(
        offline,
        &crate::project_deadlines::NewDeadline {
            project_id: project,
            title: Some("Synthetic native deadline".into()),
            description: None,
            due_at: "2026-12-01T12:00:00Z".into(),
            authority_kind: "internal".into(),
            authority: "Synthetic authority".into(),
            source: "Synthetic native input".into(),
            source_document_id: None,
            owner_person_id: Some(person),
            calendar_role: "firm".into(),
            category: Some("internal".into()),
            internal_lead_days: None,
            client_lead_days: None,
            replay_key: None,
        },
    )
    .await
    .unwrap();
}

async fn check_remaining_replay(offline: &SurrealDb, pg: &PostgresDb) {
    let approval: Uuid = sqlx::query_scalar("SELECT id FROM summary_sender_approval")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let event: Uuid = sqlx::query_scalar("SELECT id FROM summary_sender_approval_event")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let deadline: Uuid = sqlx::query_scalar("SELECT id FROM project_deadlines")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    for key in [
        MirrorKey::SummarySenderApproval(approval),
        MirrorKey::SummarySenderApprovalEvent(event),
    ] {
        assert_eq!(
            communications::mirror(offline, pg, &key).await.unwrap(),
            Mirrored::Unchanged
        );
    }
    assert!(communications::parity(offline, pg)
        .await
        .unwrap()
        .tables
        .is_empty());
    assert!(communications::reconcile(offline, pg)
        .await
        .unwrap()
        .tables
        .is_empty());
    assert_eq!(
        deadlines::mirror(offline, pg, &MirrorKey::ProjectDeadline(deadline))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    assert!(deadlines::parity(offline, pg)
        .await
        .unwrap()
        .tables
        .is_empty());
    assert!(deadlines::reconcile(offline, pg)
        .await
        .unwrap()
        .tables
        .is_empty());
}
