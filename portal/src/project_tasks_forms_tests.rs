use super::*;
use axum::http::StatusCode;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use store::persons::{NewPerson, Role};

#[tokio::test]
async fn native_task_actions_cannot_change_a_different_matter() {
    if std::env::var("NAV_TASK_FORM_CHILD").as_deref() == Ok("1") {
        Box::pin(cross_matter_actions()).await;
        return;
    }
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL fixture required"
        );
        return;
    };
    let admin = sqlx::PgPool::connect_with(
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database("smoke")
            .ssl_mode(PgSslMode::Disable),
    )
    .await
    .unwrap();
    let database = format!("task_forms_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "project_tasks_forms::tests::native_task_actions_cannot_change_a_different_matter",
            "--nocapture",
        ])
        .env("NAV_TASK_FORM_CHILD", "1")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", &port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", &database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .status()
        .unwrap();
    sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1")
        .bind(&database)
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    assert!(
        status.success(),
        "cross-matter native form regression failed"
    );
}

async fn participant(db: &store::surreal::SurrealDb, project_id: Uuid) -> Uuid {
    let person = store::persons::create(
        db,
        &NewPerson::with_role(
            "Synthetic task lawyer",
            format!("{}@example.com", Uuid::new_v4()),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        db,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: person.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    person.id
}

async fn source_project(db: &store::surreal::SurrealDb, name: &str) -> store::projects::Project {
    let unique = Uuid::new_v4();
    let jurisdiction = store::jurisdictions::create(
        db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic task jurisdiction {unique}"),
            format!("TASK-{unique}"),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type =
        store::entity_types::create(db, &format!("Synthetic task entity type {unique}"))
            .await
            .unwrap();
    let entity = store::entities::create(
        db,
        &store::entities::NewEntity {
            name: format!("Synthetic task entity {unique}"),
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
    store::projects::create(
        db,
        &store::projects::NewProject {
            code: store::projects::code_from_name(name, unique),
            name: name.into(),
            entity_id: entity.id,
            status: "open".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

async fn cross_matter_actions() {
    let db = store::test_support::mem_surreal().await;
    let own = source_project(&db, "Synthetic allowed matter").await;
    let foreign = source_project(&db, "Synthetic other matter").await;
    let actor = participant(&db, own.id).await;
    let other = participant(&db, foreign.id).await;
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    crate::project_tasks_api::prepared_store(&db, "create", foreign.id, None)
        .await
        .unwrap();
    let task = store::tasks::create(
        &pg,
        foreign.id,
        other,
        &store::tasks::NewProjectTask {
            title: "Synthetic retained work".into(),
            body: None,
            assignee_id: Some(other),
            due_on: None,
        },
    )
    .await
    .unwrap();
    let session = SessionData {
        person_id: Some(actor),
        ..SessionData::fresh("synthetic@example.com", Role::Lawyer)
    };
    let response = Box::pin(assign(
        State(db.clone()),
        Some(Extension(session.clone())),
        Path((own.code.clone(), task.id)),
        Form(TaskAssignForm {
            assignee_id: String::new(),
        }),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = Box::pin(transition(
        State(db),
        Some(Extension(session)),
        Path((own.code, task.id)),
        Form(TaskTransitionForm {
            next_state: "in_progress".into(),
        }),
    ))
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let retained = store::tasks::find_by_id(&pg, task.id).await.unwrap();
    assert_eq!(retained.assignee_id, Some(other));
    assert_eq!(retained.state, store::tasks::TaskState::Todo);
    assert_eq!(
        store::tasks::list_events(&pg, task.id).await.unwrap().len(),
        1
    );
}

#[tokio::test]
async fn transition_form_accepts_the_rendered_forward_action_fields() {
    use axum::extract::FromRequest;
    for state in ["in_progress", "done"] {
        let request = axum::http::Request::builder()
            .method(axum::http::Method::POST)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(axum::body::Body::from(format!(
                "_csrf=synthetic&next_state={state}"
            )))
            .unwrap();
        let Form(form) = Form::<TaskTransitionForm>::from_request(request, &())
            .await
            .unwrap();
        assert_eq!(form.next_state, state);
    }
}

#[test]
fn only_an_explicitly_empty_assignee_clears_an_assignment() {
    assert_eq!(parse_assignee(" ").unwrap(), None);
    assert_eq!(
        parse_assignee(&Uuid::nil().to_string()).unwrap(),
        Some(Uuid::nil())
    );
    assert!(parse_assignee("not-a-person-id").is_err());
}

#[test]
fn invalid_task_fields_explain_how_to_correct_the_input() {
    for (field, limit) in [("title", "300 bytes"), ("body", "2000 bytes")] {
        let message = flash_for(&store::tasks::ProjectTaskError::Invalid(field));
        assert!(message.contains(limit), "{message}");
        assert!(!message.contains("unavailable"), "{message}");
    }
}
