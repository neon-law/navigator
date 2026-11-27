//! The self-serve formation walk, on PostgreSQL alone.
use super::tests::{client, location, onboarding_catalog, session};
use super::*;
use axum::http::StatusCode;

#[tokio::test]
async fn personal_formations_resume_independently_without_a_lawyer() {
    store::test_support::with_postgres(async {
        let db = store::application_store::ApplicationStore::uninitialized();
        let app = crate::test_support::app_state(db.clone()).await;
        store::seed::seed_canonical(&db, &app.storage)
            .await
            .unwrap();
        let owner = client(&db, "Test Founder", "founder@example.com").await;
        let other = client(&db, "Other Founder", "other-founder@example.com").await;
        let door = DoorState {
            app: app.clone(),
            catalog: Some(onboarding_catalog()),
            notations: Some(std::sync::Arc::new(vec![NotationOffer {
                slug: "nevada-llc-formation".into(),
                title: "Nevada LLC Formation".into(),
                template_code: store::personal_projects::FORMATION.into(),
            }])),
        };
        let start = |seen| {
            post_start(
                State(door.clone()),
                Extension(session(&owner)),
                Path("nevada-llc-formation".to_owned()),
                axum::http::HeaderMap::new(),
                Form(StartForm {
                    seen_notation: seen,
                    ..Default::default()
                }),
            )
        };
        let (one, retry) = tokio::join!(start(None), start(None));
        let first = location(&one);
        assert_eq!(one.status(), StatusCode::SEE_OTHER);
        assert_eq!(first, location(&retry));
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let rows = store::personal_projects::formations(&pg, owner.id, "neon")
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let id = rows[0].id;
        assert_eq!(rows[0].intake_path(), first);
        let notation = store::notations::find_by_id(&db, id)
            .await
            .unwrap()
            .unwrap();
        check_founder_and_onboarding(&db, notation.project_id, owner.id).await;
        let answer = |question, value| {
            workflows::notation_session::record_client_answer_with_reference(
                &db,
                Some(&app.storage),
                id,
                question,
                value,
                None,
                owner.id,
            )
        };
        answer("person__client", "Test Founder").await.unwrap();
        answer("entity__company", "First Example LLC")
            .await
            .unwrap();
        let reopened = location(&start(None).await);
        assert_eq!(reopened, first);
        let workflows::notation_session::ClientIntakeStep::NeedsAnswer { question, .. } =
            workflows::notation_session::client_intake_step(&db, Some(&app.storage), id)
                .await
                .unwrap()
        else {
            panic!("saved intake has another question")
        };
        assert_eq!(question.code, "person__registered_agent");
        let second = location(&start(Some(id)).await);
        assert_ne!(second, first);
        let both = store::personal_projects::formations(&pg, owner.id, "neon")
            .await
            .unwrap();
        assert_eq!(both.len(), 2);
        assert_eq!(both[0].project_code, both[1].project_code);
        assert_eq!(both[1].label, "First Example LLC");
        assert_eq!(
            location(&start(None).await),
            "/notations/nevada-llc-formation/start"
        );
        check_isolation_and_move(&pg, owner.id, other.id, notation.project_id, id, &second).await;
    })
    .await;
}
async fn check_founder_and_onboarding(
    db: &store::application_store::ApplicationStore,
    project_id: Uuid,
    owner: Uuid,
) {
    let project = store::projects::find_by_id(db, project_id)
        .await
        .unwrap()
        .unwrap();
    for (code, exempt) in [
        ("navigator", true),
        ("navigatorbusiness", true),
        ("my-navigator", false),
    ] {
        let comparison = store::projects::create(
            db,
            &store::projects::NewProject {
                code: code.into(),
                name: "Example business".into(),
                entity_id: project.entity_id,
                firm_id: project.firm_id,
                brand: "neon".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let result = workflows::enforce_engagement_first(
            db,
            comparison.id,
            store::personal_projects::FORMATION,
        )
        .await;
        if exempt {
            result.unwrap();
        } else {
            assert!(matches!(
                result,
                Err(
                    workflows::notation_session::NotationSessionError::EngagementMustBeFirst { .. }
                )
            ));
        }
    }
    let roles = store::projects::participations_for_project(db, project_id)
        .await
        .unwrap();
    assert_eq!(roles.len(), 1);
    assert!(roles[0].is_client_dri && !roles[0].is_lawyer_dri);
    assert_eq!(roles[0].person_id, owner);
}

async fn check_isolation_and_move(
    pg: &store::postgres::PostgresDb,
    owner: Uuid,
    other: Uuid,
    source: Uuid,
    id: Uuid,
    second: &str,
) {
    assert!(store::personal_projects::formations(pg, other, "neon")
        .await
        .unwrap()
        .is_empty());
    assert!(store::personal_projects::formations(pg, owner, "daybridge")
        .await
        .unwrap()
        .is_empty());
    let move_draft = |person| store::personal_projects::separate_formation(pg, person, "neon", id);
    assert!(move_draft(other).await.unwrap().is_none());
    let moved = move_draft(owner).await.unwrap().unwrap();
    assert_eq!(move_draft(owner).await.unwrap(), Some(moved.clone()));
    let rows = store::personal_projects::formations(pg, owner, "neon")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].intake_path(), moved);
    assert_eq!(rows[1].label, "First Example LLC");
    assert_eq!(rows[0].intake_path(), second);
    let bookmark = |person| store::personal_projects::moved_intake(source, id, person, "neon");
    assert_eq!(bookmark(owner).await.unwrap(), Some(moved));
    assert!(bookmark(other).await.unwrap().is_none());
    assert_eq!(
        store::personal_projects::owner_access(source, other, "neon")
            .await
            .unwrap(),
        Some(false)
    );
}
