//! Inbox Project choices must use the admitted native authority without Source.
use store::application_store::ApplicationStore;
use store::inbox::{self, InboxError, ProjectChoice};
use store::persons::Role;
use store::postgres::PostgresDb;
use store::projects;
use uuid::Uuid;

async fn entity(db: &ApplicationStore) -> Uuid {
    let id = Uuid::new_v4();
    let jurisdiction = store::jurisdictions::create(
        db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {id}"),
            id.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    let kind = store::entity_types::create(db, &format!("Synthetic kind {id}"))
        .await
        .unwrap();
    store::entities::create(
        db,
        &store::entities::NewEntity {
            name: format!("Synthetic entity {id}"),
            entity_type_id: kind.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
    .id
}

async fn fixture(db: &ApplicationStore) -> (Uuid, Uuid) {
    let admin = store::persons::create(
        db,
        &store::persons::NewPerson::with_role(
            "Synthetic administrator",
            format!("synthetic-{}@example.com", Uuid::new_v4()),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        db,
        &store::firms::NewFirm {
            name: format!("Firm {}", Uuid::new_v4().simple()),
            status: "active".to_owned(),
            entity_id: entity(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let key = format!("synthetic-{}", Uuid::new_v4().simple());
    store::brands::create(
        db,
        Role::Admin,
        Some(admin.id),
        &store::brands::NewBrand {
            name: format!("Brand {}", Uuid::new_v4().simple()),
            key: key.clone(),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::firms::attach_brand(db, Role::Owner, None, firm.id, &key)
        .await
        .unwrap();
    let project = projects::create(
        db,
        &projects::NewProject {
            code: format!("synthetic-{}", Uuid::new_v4().simple()),
            name: "Zulu matter".to_owned(),
            status: "open".to_owned(),
            entity_id: entity(db).await,
            firm_id: Some(firm.id),
            brand: key,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (project.id, admin.id)
}

async fn additional_project(
    db: &ApplicationStore,
    base: &projects::Project,
    name: &str,
    status: &str,
    description: Option<&str>,
) -> projects::Project {
    projects::create(
        db,
        &projects::NewProject {
            code: format!("synthetic-{}", Uuid::new_v4().simple()),
            name: name.into(),
            status: status.into(),
            entity_id: base.entity_id,
            firm_id: base.firm_id,
            brand: base.brand.clone(),
            description: description.map(str::to_owned),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn project_choices_use_native_firm_filter_order_and_complete_row_shape() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (id, _) = fixture(&db).await;
        let base = projects::find_by_id(&db, id).await.unwrap().unwrap();
        // Open-only picker: the excluded rows prove archived and closed
        // matters stay out; alpha carries the description for row shape.
        let alpha = additional_project(
            &db,
            &base,
            "Alpha matter",
            "open",
            Some("Synthetic assignment context."),
        )
        .await;
        additional_project(&db, &base, "Archived matter", "archived", None).await;
        additional_project(
            &db,
            &base,
            "Closed matter",
            "closed",
            Some("Must stay excluded."),
        )
        .await;
        let (other_id, _) = fixture(&db).await;
        let other = projects::find_by_id(&db, other_id).await.unwrap().unwrap();
        assert_ne!(base.firm_id, other.firm_id);
        let choices = inbox::projects_in_firm(&db, base.firm_id.unwrap())
            .await
            .unwrap();
        assert_eq!(
            choices,
            vec![
                ProjectChoice {
                    id: alpha.id,
                    code: alpha.code,
                    name: alpha.name,
                    description: alpha.description
                },
                ProjectChoice {
                    id: base.id,
                    code: base.code,
                    name: base.name,
                    description: None
                },
            ]
        );
        let choices = inbox::projects_in_firm(&db, other.firm_id.unwrap())
            .await
            .unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].id, other_id);
        assert!(inbox::projects_in_firm(&db, Uuid::new_v4())
            .await
            .unwrap()
            .is_empty());
    })
    .await;
}

#[tokio::test]
async fn native_intake_detail_includes_project_choices_without_source() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (project, admin) = fixture(&db).await;
        let base = projects::find_by_id(&db, project).await.unwrap().unwrap();
        let received = inbox::ingest_inbound(
            &db,
            &inbox::IngestInbound {
                firm_id: base.firm_id.unwrap(),
                project_id: None,
                brand_key: Some(&base.brand),
                interaction_type: None,
                provider_post_id: None,
                parent_provider_message_id: None,
                visibility: None,
                channel: inbox::channel::EMAIL,
                provider_account: "synthetic-inbox",
                provider_message_id: "synthetic-inbox-project-choices",
                provider_thread_ref: None,
                external_address: "synthetic-sender@example.invalid",
                external_name: None,
                subject: Some("Synthetic Project selection"),
                body: "Synthetic inbound text.",
                source_page: None,
                source_channel: None,
                matter_consent: true,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachments: vec![],
                occurred_at: "2026-11-19T00:00:00Z",
                raw_storage_key: None,
            },
        )
        .await
        .unwrap();
        let detail = inbox::intake_detail(&db, admin, received.conversation_id)
            .await
            .unwrap();
        assert_eq!(detail.conversation.firm_id, base.firm_id.unwrap());
        assert!(detail.bodies_visible);
        assert_eq!(detail.messages.len(), 1);
        assert_eq!(
            detail.projects,
            vec![ProjectChoice {
                id: base.id,
                code: base.code,
                name: base.name,
                description: base.description
            }]
        );
        let outsider = store::persons::create(
            &db,
            &store::persons::NewPerson::with_role(
                "Synthetic outsider",
                format!("outside-{}@example.invalid", Uuid::new_v4()),
                Role::Lawyer,
            ),
        )
        .await
        .unwrap();
        assert!(matches!(
            inbox::intake_detail(&db, outsider.id, received.conversation_id).await,
            Err(InboxError::Denied)
        ));
    })
    .await;
}

#[tokio::test]
async fn unavailable_native_project_choices_do_not_fall_back_to_source() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (project, _) = fixture(&db).await;
        let base = projects::find_by_id(&db, project).await.unwrap().unwrap();
        let pg = PostgresDb::application().await.unwrap();
        let observer = sqlx::PgPool::connect_with(pg.pool().connect_options().as_ref().clone())
            .await
            .unwrap();
        pg.pool().close().await;
        assert!(
            matches!(inbox::projects_in_firm(&db, base.firm_id.unwrap()).await,
            Err(InboxError::Db(message)) if message == "PostgreSQL is unavailable for the Inbox")
        );
        let remains: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project WHERE id=$1)")
            .bind(project)
            .fetch_one(&observer)
            .await
            .unwrap();
        assert!(remains);
        observer.close().await;
    })
    .await;
}
