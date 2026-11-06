#![allow(clippy::doc_markdown)]
//! Integration tests for notation review, Re-ask, and their shared answer authority.
//! Native forms and REST doors enforce scope, state, completeness, and completed-route
//! correction guards. Real intake completion and review rendering keep extracted
//! proposals, confirmed branch choices, typed bindings, and frozen choice labels scoped.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use portal::AppState;
use store::persons::Role;
use store::seed;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;
use workflows::{DispatchingRuntime, InMemoryRuntime, StateMachineRuntime};

#[path = "common/bearer.rs"]
mod bearer_common;

use bearer_common::bearer;

const TEMPLATE_CODE: &str = "onboarding__letter";

struct Fixture {
    storage: Arc<dyn cloud::StorageService>,
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    notation_id: Uuid,
    /// A lawyer who participates in the notation's matter — the authorised actor.
    acting: String,
    /// A lawyer with no participation in this matter.
    outsider: String,
    /// A client-tier caller.
    client: String,
}

async fn build_fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("navigator-api-reask-{}", Uuid::now_v7())),
        )
        .await
        .unwrap(),
    );
    seed::seed_canonical(&surreal, &storage).await.unwrap();
    let tmpl = store::templates::resolve(&surreal, None, TEMPLATE_CODE)
        .await
        .unwrap()
        .expect("seed inserts onboarding__letter");
    let client_person = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Libra", "libra@example.com", Role::Client),
    )
    .await
    .unwrap();
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("libra-retainer-{}", Uuid::now_v7()),
            name: "Libra retainer".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    // The acting lawyer participates in the matter; the outsider does not.
    let acting_person = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Acting Lawyer", "acting@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project.id, acting_person.id, "lawyer")
        .await
        .unwrap();
    let outsider_person = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Outsider", "outsider@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let snapshot = workflows::notation_session::questionnaire_snapshot_for_template(
        &surreal,
        Some(&storage),
        &tmpl,
    )
    .await
    .unwrap();
    let notation_id = store::notations::create(
        &surreal,
        &store::notations::NewNotation::new(tmpl.id, client_person.id, project.id, "BEGIN")
            .with_questionnaire_snapshot(snapshot),
    )
    .await
    .unwrap()
    .id;

    let runtime = Arc::new(InMemoryRuntime::new());
    let email: Arc<dyn portal::email::EmailService> =
        Arc::new(portal::email::CapturingEmail::new());
    let workflow_runtime: Arc<dyn StateMachineRuntime> = Arc::new(DispatchingRuntime::new(
        runtime.clone(),
        email.clone(),
        storage.clone(),
    ));
    let state = AppState {
        storage: storage.clone(),
        workflow_runtime,
        questionnaire_runtime: runtime,
        email,
        ..portal::test_support::app_state(surreal.clone()).await
    };
    Fixture {
        storage,
        app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        surreal,
        notation_id,
        acting: bearer(acting_person.id, Role::Lawyer),
        outsider: bearer(outsider_person.id, Role::Lawyer),
        client: bearer(client_person.id, Role::Client),
    }
}

/// Drive the eight-question retainer intake to the `lawyer_review` gate over
/// the lawyer walk, mirroring `reask_handler.rs`.
async fn walk_to_lawyer_review(fx: &Fixture) {
    for value in [
        "Libra%20Holdings%20LLC",
        "500%20Innovation%20Way%20Reno%20NV%2089501",
        "Libra",
        "Firm%20Principal",
        "Estate%20plan",
        "2026-09-01",
        "Draft%20and%20file%20the%20matter%20documents.",
        "nevada",
    ] {
        let resp = fx
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/app/lawyer/notations/{}/step", fx.notation_id))
                    .header("authorization", &fx.acting)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("value={value}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            resp.status() == StatusCode::SEE_OTHER || resp.status() == StatusCode::OK,
            "walk step {value} returned {}",
            resp.status()
        );
    }
    assert_eq!(state_of(fx).await, "lawyer_review");
}

async fn state_of(fx: &Fixture) -> String {
    store::notations::find_by_id(&fx.surreal, fx.notation_id)
        .await
        .unwrap()
        .unwrap()
        .state
}

async fn post_json(
    fx: &Fixture,
    path_suffix: &str,
    auth: Option<&str>,
    body: serde_json::Value,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!(
            "/app/api/notations/{}/{path_suffix}",
            fx.notation_id
        ))
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn a_participant_lawyer_requests_changes_then_reasks() {
    let fx = build_fixture().await;
    walk_to_lawyer_review(&fx).await;

    let resp = post_json(
        &fx,
        "request-changes",
        Some(&fx.acting),
        serde_json::json!({ "flagged": ["person__client"], "note": "Confirm the legal name" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        state_of(&fx).await,
        "reask__client",
        "request-changes parks the matter at reask__client"
    );

    let resp = post_json(
        &fx,
        "reask",
        Some(&fx.acting),
        serde_json::json!({ "answers": { "person__client": "Libra Jones" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        state_of(&fx).await,
        "lawyer_review",
        "reask loops the matter back to review"
    );
}

#[tokio::test]
async fn request_changes_rejects_the_unauthorized() {
    let fx = build_fixture().await;
    walk_to_lawyer_review(&fx).await;
    let body = serde_json::json!({ "flagged": ["person__client"] });

    let client = post_json(&fx, "request-changes", Some(&fx.client), body.clone()).await;
    assert_eq!(
        client.status(),
        StatusCode::FORBIDDEN,
        "client tier is refused"
    );

    let anon = post_json(&fx, "request-changes", None, body.clone()).await;
    assert_eq!(
        anon.status(),
        StatusCode::UNAUTHORIZED,
        "anonymous is refused"
    );

    let outsider = post_json(&fx, "request-changes", Some(&fx.outsider), body).await;
    assert_eq!(
        outsider.status(),
        StatusCode::NOT_FOUND,
        "a lawyer outside the matter gets a non-disclosing 404"
    );
    assert_eq!(
        state_of(&fx).await,
        "lawyer_review",
        "no refusal moved the matter"
    );
}

#[tokio::test]
async fn request_changes_off_gate_is_409() {
    // Still at BEGIN — not awaiting review.
    let fx = build_fixture().await;
    let resp = post_json(
        &fx,
        "request-changes",
        Some(&fx.acting),
        serde_json::json!({ "flagged": ["person__client"] }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn request_changes_with_no_flags_is_400() {
    let fx = build_fixture().await;
    walk_to_lawyer_review(&fx).await;
    let resp = post_json(
        &fx,
        "request-changes",
        Some(&fx.acting),
        serde_json::json!({ "flagged": [], "note": "fix something" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        state_of(&fx).await,
        "lawyer_review",
        "a flagless request moves nothing"
    );
}

#[tokio::test]
async fn reask_off_gate_is_409() {
    // Still at BEGIN — not awaiting re-collection.
    let fx = build_fixture().await;
    let resp = post_json(
        &fx,
        "reask",
        Some(&fx.acting),
        serde_json::json!({ "answers": { "person__client": "Libra Jones" } }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn reask_with_a_blank_flagged_answer_is_400() {
    let fx = build_fixture().await;
    walk_to_lawyer_review(&fx).await;
    // Park at reask__client with person__client flagged.
    let parked = post_json(
        &fx,
        "request-changes",
        Some(&fx.acting),
        serde_json::json!({ "flagged": ["person__client"] }),
    )
    .await;
    assert_eq!(parked.status(), StatusCode::NO_CONTENT);

    // Resubmit without re-collecting the flagged answer: refused whole.
    let resp = post_json(
        &fx,
        "reask",
        Some(&fx.acting),
        serde_json::json!({ "answers": {} }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        state_of(&fx).await,
        "reask__client",
        "an incomplete resubmit leaves the matter parked"
    );
}

async fn branch_review_fixture() -> Fixture {
    let fx = complete_proposed_branch().await;
    let notation = store::notations::find_by_id(&fx.surreal, fx.notation_id)
        .await
        .unwrap()
        .unwrap();
    let question = store::questions::find_by_code(&fx.surreal, "custom_text")
        .await
        .unwrap()
        .unwrap();
    store::answers::record(
        &fx.surreal,
        &store::answers::NewAnswer::new(
            question.id,
            notation.person_id,
            store::answers::primitive("Stale off-route answer"),
        )
        .in_notation(fx.notation_id, "custom_text__b"),
    )
    .await
    .unwrap();
    fx
}

#[tokio::test]
async fn change_request_refuses_an_off_route_answer_even_when_it_was_previously_recorded() {
    use http_body_util::BodyExt;
    let fx = branch_review_fixture().await;
    let response = post_json(
        &fx,
        "request-changes",
        Some(&fx.acting),
        serde_json::json!({"flagged": ["custom_text__b"]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "question_not_on_route");
    assert_eq!(state_of(&fx).await, "lawyer_review");
    assert!(store::reask::flagged_questions(&fx.surreal, fx.notation_id)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn reask_refuses_a_route_change_before_any_answer_is_written() {
    use http_body_util::BodyExt;
    let fx = branch_review_fixture().await;
    let response = post_json(
        &fx,
        "request-changes",
        Some(&fx.acting),
        serde_json::json!({"flagged": ["custom_text__a", "custom_single_choice__path"]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let before = store::answers::for_notation(&fx.surreal, fx.notation_id)
        .await
        .unwrap();
    let response = post_json(&fx, "reask", Some(&fx.acting), serde_json::json!({"answers": {"custom_text__a": "Must not be written", "custom_single_choice__path": "b"}})).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "questionnaire_route_changed");
    assert!(payload["message"]
        .as_str()
        .unwrap()
        .contains("fresh intake"));
    assert_eq!(
        store::answers::for_notation(&fx.surreal, fx.notation_id)
            .await
            .unwrap(),
        before
    );
    assert_eq!(state_of(&fx).await, "reask__client");
    // A different choice leading to the same question chain is a correction.
    let response = post_json(&fx, "reask", Some(&fx.acting), serde_json::json!({"answers": {"custom_text__a": "Corrected", "custom_single_choice__path": "c"}})).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(state_of(&fx).await, "lawyer_review");
}

async fn stale_flag_fixture() -> Fixture {
    let fx = branch_review_fixture().await;
    store::notations::update_state(&fx.surreal, fx.notation_id, "reask__client")
        .await
        .unwrap();
    let actor = store::persons::find_by_email_ci(&fx.surreal, "acting@example.com")
        .await
        .unwrap()
        .unwrap()
        .id;
    store::reask::record_change_request(
        &fx.surreal,
        fx.notation_id,
        actor,
        &["custom_text__b".into()],
        None,
    )
    .await
    .unwrap();
    fx
}

#[tokio::test]
async fn stale_off_route_inputs_are_not_offered_on_the_reask_page() {
    use http_body_util::BodyExt;
    let fx = stale_flag_fixture().await;
    let response = fx
        .app
        .oneshot(
            Request::builder()
                .uri(format!("/app/lawyer/notations/{}/reask", fx.notation_id))
                .header("authorization", &fx.acting)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("fresh intake"));
}

#[tokio::test]
async fn stale_off_route_flags_cannot_authorize_recollection() {
    use http_body_util::BodyExt;
    let fx = stale_flag_fixture().await;
    let before = store::answers::for_notation(&fx.surreal, fx.notation_id)
        .await
        .unwrap();
    let response = post_json(
        &fx,
        "reask",
        Some(&fx.acting),
        serde_json::json!({"answers": {"custom_text__b": "Must not replace stale answer"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "question_not_on_route");
    assert_eq!(
        store::answers::for_notation(&fx.surreal, fx.notation_id)
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn individual_reask_writer_cannot_bypass_the_route_guard() {
    let fx = branch_review_fixture().await;
    let actor = store::persons::find_by_email_ci(&fx.surreal, "acting@example.com")
        .await
        .unwrap()
        .unwrap()
        .id;
    store::reask::record_change_request(
        &fx.surreal,
        fx.notation_id,
        actor,
        &["custom_text__b".into(), "custom_single_choice__path".into()],
        None,
    )
    .await
    .unwrap();
    let before = store::answers::for_notation(&fx.surreal, fx.notation_id)
        .await
        .unwrap();
    for (code, value) in [
        ("custom_text__b", "Off-route"),
        ("custom_single_choice__path", "b"),
    ] {
        let result = workflows::notation_session::record_reask_answer(
            &fx.surreal,
            Some(&fx.storage),
            fx.notation_id,
            code,
            value,
            None,
            workflows::notation_session::AnswerAuthor::lawyer(Some(actor)),
        )
        .await;
        assert!(
            result.is_err(),
            "a direct writer must preserve the completed route"
        );
        assert_eq!(
            store::answers::for_notation(&fx.surreal, fx.notation_id)
                .await
                .unwrap(),
            before
        );
    }
}

async fn confirm_native_branch(fx: &Fixture, choice: &str) {
    for value in [choice, "Confirmed"] {
        let response = fx
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/app/lawyer/notations/{}/step", fx.notation_id))
                    .header("authorization", &fx.acting)
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(format!("value={value}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response.status() == StatusCode::SEE_OTHER || response.status() == StatusCode::OK);
    }
    assert_eq!(state_of(fx).await, "lawyer_review");
}

async fn complete_proposed_branch() -> Fixture {
    let fx = render_fixture(ROUTE_BODY, true).await;
    assert!(workflows::notation_session::record_extracted_answer(
        &fx.surreal,
        None,
        fx.notation_id,
        "custom_single_choice__path",
        "a"
    )
    .await
    .unwrap());
    confirm_native_branch(&fx, "a").await;
    fx
}

#[tokio::test]
async fn a_late_proposal_cannot_replace_a_confirmed_completed_branch() {
    let fx = complete_proposed_branch().await;
    assert!(workflows::notation_session::record_extracted_answer(
        &fx.surreal,
        None,
        fx.notation_id,
        "custom_single_choice__path",
        "b"
    )
    .await
    .unwrap());
    let projected = workflows::notation_session::questionnaire_chain_for_notation(
        &fx.surreal,
        None,
        fx.notation_id,
    )
    .await
    .unwrap();
    assert!(
        projected.contains(&"custom_text__b".into()),
        "the ordinary walker still reads proposals"
    );
    let confirmed = workflows::notation_session::questionnaire_chain_for_reask(
        &fx.surreal,
        Some(&fx.storage),
        fx.notation_id,
    )
    .await
    .unwrap();
    assert_eq!(confirmed, ["custom_single_choice__path", "custom_text__a"]);
    let actor = store::persons::find_by_email_ci(&fx.surreal, "acting@example.com")
        .await
        .unwrap()
        .unwrap()
        .id;
    store::reask::record_change_request(
        &fx.surreal,
        fx.notation_id,
        actor,
        &["custom_text__a".into(), "custom_text__b".into()],
        None,
    )
    .await
    .unwrap();
    let before = store::answers::for_notation(&fx.surreal, fx.notation_id)
        .await
        .unwrap();
    let refused = workflows::notation_session::record_reask_answer(
        &fx.surreal,
        Some(&fx.storage),
        fx.notation_id,
        "custom_text__b",
        "Must not be collected",
        None,
        workflows::notation_session::AnswerAuthor::lawyer(Some(actor)),
    )
    .await;
    assert!(refused.is_err());
    assert_eq!(
        store::answers::for_notation(&fx.surreal, fx.notation_id)
            .await
            .unwrap(),
        before
    );
    workflows::notation_session::record_reask_answer(
        &fx.surreal,
        Some(&fx.storage),
        fx.notation_id,
        "custom_text__a",
        "Corrected confirmed answer",
        None,
        workflows::notation_session::AnswerAuthor::lawyer(Some(actor)),
    )
    .await
    .unwrap();
}

const ROUTE_BODY: &str = "# Synthetic document\n\n{{#if custom_single_choice__path=b}}OTHER: {{custom_text__b}}{{else}}CHOSEN: {{custom_text__a}}{{/if}}\n\n{{#if custom_text__b}}STALE_B_PRESENT{{/if}}\n\n{{#if person__client.email}}PERSON_PRESENT{{/if}}\n\n{{#if custom_single_choice__governing_law=nevada}}OFF_ROUTE_LAW{{/if}}\n";

fn branch_snapshot() -> serde_json::Value {
    serde_json::json!({
        "spec": {
            "BEGIN": {"_": "custom_single_choice__path"},
            "custom_single_choice__path": {"a": "custom_text__a", "b": "custom_text__b", "c": "custom_text__a"},
            "custom_text__a": {"_": "END"},
            "custom_text__b": {"_": "person__client"},
            "person__client": {"_": "custom_single_choice__governing_law"},
            "custom_single_choice__governing_law": {"_": "END"}, "END": {}
        },
        "prompts": {}, "choices": {"path": {"a": "A", "b": "B", "c": "Also A"}, "governing_law": {"nevada": "Nevada"}}
    })
}

async fn render_fixture(body: &str, questionnaire: bool) -> Fixture {
    render_fixture_with_snapshot(body, questionnaire.then(branch_snapshot), "render_fixture").await
}

async fn render_fixture_with_snapshot(
    body: &str,
    snapshot: Option<serde_json::Value>,
    code: &str,
) -> Fixture {
    let mut fx = build_fixture().await;
    let original = store::notations::find_by_id(&fx.surreal, fx.notation_id)
        .await
        .unwrap()
        .unwrap();
    let source = if let Some(snapshot) = &snapshot {
        let mut metadata = snapshot.clone();
        metadata["questionnaire"] = metadata.as_object_mut().unwrap().remove("spec").unwrap();
        metadata["kind"] = "letter".into();
        metadata["title"] = "Synthetic render".into();
        metadata["workflow"] = serde_json::json!({"BEGIN": {"created": "lawyer_review"}, "lawyer_review": {"approved": "END", "changes_requested": "reask__client"}, "reask__client": {"intake_resubmitted": "lawyer_review"}, "END": {}});
        format!(
            "---\n{}---\n{body}",
            serde_yaml::to_string(&metadata).unwrap()
        )
    } else {
        body.to_string()
    };
    let asset =
        store::assets::ingest_content(&fx.surreal, &fx.storage, source.as_bytes(), "text/markdown")
            .await
            .unwrap();
    let template = store::templates::save_version(
        &fx.surreal,
        Some(original.project_id),
        code,
        store::templates::Version {
            title: "Synthetic render".into(),
            respondent_type: "person".into(),
            asset_id: Some(asset),
            form_code: None,
            kind: Some("letter".into()),
            source_commit_sha: Some("synthetic-render".into()),
        },
    )
    .await
    .unwrap()
    .into_model();
    let mut notation = store::notations::NewNotation::new(
        template.id,
        original.person_id,
        original.project_id,
        "BEGIN",
    );
    if let Some(snapshot) = snapshot {
        notation = notation.with_questionnaire_snapshot(snapshot);
    }
    fx.notation_id = store::notations::create(&fx.surreal, &notation)
        .await
        .unwrap()
        .id;
    fx
}

async fn reviewed_html(fx: &Fixture) -> String {
    use http_body_util::BodyExt;
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/app/lawyer/notations/{}/review", fx.notation_id))
                .header("authorization", &fx.acting)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn completed_body_uses_confirmed_route_and_excludes_stale_answers_and_identity_defaults() {
    let fx = render_fixture(ROUTE_BODY, true).await;
    for (code, value) in [
        ("custom_single_choice__path", "b"),
        ("custom_text__b", "PendingProposal"),
    ] {
        assert!(workflows::notation_session::record_extracted_answer(
            &fx.surreal,
            Some(&fx.storage),
            fx.notation_id,
            code,
            value
        )
        .await
        .unwrap());
    }
    assert_eq!(state_of(&fx).await, "BEGIN");
    assert!(
        reviewed_html(&fx).await.contains("OTHER: PendingProposal"),
        "active intake retains proposed defaults"
    );
    confirm_native_branch(&fx, "a").await;
    let notation = store::notations::find_by_id(&fx.surreal, fx.notation_id)
        .await
        .unwrap()
        .unwrap();
    for (code, value) in [
        ("custom_text__b", "StaleOtherRoute"),
        ("custom_single_choice__governing_law", "nevada"),
    ] {
        let question =
            store::questions::find_by_code(&fx.surreal, code.split_once("__").unwrap().0)
                .await
                .unwrap()
                .unwrap();
        store::answers::record(
            &fx.surreal,
            &store::answers::NewAnswer::new(
                question.id,
                notation.person_id,
                store::answers::primitive(value),
            )
            .in_notation(fx.notation_id, code),
        )
        .await
        .unwrap();
    }
    assert!(workflows::notation_session::record_extracted_answer(
        &fx.surreal,
        Some(&fx.storage),
        fx.notation_id,
        "custom_single_choice__path",
        "b"
    )
    .await
    .unwrap());
    let before = store::answers::for_notation(&fx.surreal, fx.notation_id)
        .await
        .unwrap();
    let rendered = reviewed_html(&fx).await;
    assert!(rendered.contains("CHOSEN: Confirmed"));
    for forbidden in [
        "StaleOtherRoute",
        "STALE_B_PRESENT",
        "PERSON_PRESENT",
        "OFF_ROUTE_LAW",
    ] {
        assert!(
            !rendered.contains(forbidden),
            "off-route body content: {forbidden}"
        );
    }
    assert_eq!(
        store::answers::for_notation(&fx.surreal, fx.notation_id)
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn a_plain_letter_without_a_questionnaire_keeps_its_bound_identity() {
    let fx = render_fixture(
        "# Letter\n\nBOUND_CLIENT: {{person__client.email}}\n",
        false,
    )
    .await;
    assert!(matches!(
        workflows::notation_session::render_answer_projection(&fx.surreal, None, fx.notation_id)
            .await,
        Err(workflows::notation_session::NotationSessionError::QuestionnaireStorageRequired)
    ));
    assert!(reviewed_html(&fx)
        .await
        .contains("BOUND_CLIENT: libra@example.com"));
}

#[tokio::test]
async fn malformed_frozen_questionnaire_does_not_fall_back_to_unscoped_body_values() {
    for (snapshot, decode_error) in [
        (serde_json::json!({"spec": "malformed"}), true),
        (
            serde_json::json!({"spec": {"BEGIN": {"_": "custom_text__a"},
            "custom_text__a": {"_": "custom_text__a"}, "END": {}}, "prompts": {}}),
            false,
        ),
    ] {
        let fx = render_fixture("# Letter\n\nBOUND_CLIENT: {{person__client.email}}\n", true).await;
        store::notations::update_questionnaire_snapshot(&fx.surreal, fx.notation_id, snapshot)
            .await
            .unwrap();
        let result = workflows::notation_session::render_answer_projection(
            &fx.surreal,
            Some(&fx.storage),
            fx.notation_id,
        )
        .await;
        if decode_error {
            assert!(matches!(
                result,
                Err(workflows::notation_session::NotationSessionError::SnapshotDecode(_))
            ));
        } else {
            assert!(matches!(
                result,
                Err(workflows::notation_session::NotationSessionError::Spec(_))
            ));
        }
        assert_review_refused(&fx, StatusCode::INTERNAL_SERVER_ERROR).await;
    }
}

#[tokio::test]
async fn frozen_and_pinned_numeric_choices_control_body_rendering() {
    let snapshot = serde_json::json!({"spec": {
        "BEGIN": {"_": "custom_single_choice__governing_law"},
        "custom_single_choice__governing_law": {"1": "custom_text__a", "2": "custom_text__b"},
        "custom_text__a": {"_": "END"}, "custom_text__b": {"_": "END"}, "END": {}
    }, "prompts": {}, "choices": {"governing_law": {"1": "Frozen first", "2": "Frozen second"}}});
    let body = "# Numeric\n\n{{#if custom_single_choice__governing_law=1}}FROZEN_LABEL: {{custom_single_choice__governing_law}} {{custom_text__a}}{{else}}{{custom_text__b}}{{/if}}\n";
    for legacy in [false, true] {
        let mut fx =
            render_fixture_with_snapshot(body, Some(snapshot.clone()), "onboarding__letter").await;
        if legacy {
            let notation = store::notations::find_by_id(&fx.surreal, fx.notation_id)
                .await
                .unwrap()
                .unwrap();
            fx.notation_id = store::notations::create(
                &fx.surreal,
                &store::notations::NewNotation::new(
                    notation.template_id,
                    notation.person_id,
                    notation.project_id,
                    "BEGIN",
                ),
            )
            .await
            .unwrap()
            .id;
        }
        confirm_native_branch(&fx, "1").await;
        let rendered = reviewed_html(&fx).await;
        assert!(
            rendered.contains("FROZEN_LABEL: Frozen first Confirmed"),
            "legacy={legacy}"
        );
        assert!(!rendered.contains("custom_text__b"));
    }
}

#[tokio::test]
async fn unavailable_or_corrupt_pinned_body_does_not_become_questionnaire_absence() {
    for missing in [true, false] {
        let fx = render_fixture(
            "# Letter\n\nBOUND_CLIENT: {{person__client.email}}\n",
            false,
        )
        .await;
        let notation = store::notations::find_by_id(&fx.surreal, fx.notation_id)
            .await
            .unwrap()
            .unwrap();
        let template = store::templates::find_by_id(&fx.surreal, notation.template_id)
            .await
            .unwrap()
            .unwrap();
        let asset = store::assets::find_by_id(&fx.surreal, template.asset_id.unwrap())
            .await
            .unwrap()
            .unwrap();
        if missing {
            fx.storage.delete(&asset.storage_key).await.unwrap();
        } else {
            fx.storage
                .put(
                    &asset.storage_key,
                    b"tampered synthetic template",
                    "text/markdown",
                )
                .await
                .unwrap();
        }
        assert!(matches!(
            workflows::notation_session::render_answer_projection(
                &fx.surreal,
                Some(&fx.storage),
                fx.notation_id
            )
            .await,
            Err(workflows::notation_session::NotationSessionError::TemplateBody(_))
        ));
        assert_review_refused(&fx, StatusCode::INTERNAL_SERVER_ERROR).await;
    }
}

#[tokio::test]
async fn malformed_legacy_frontmatter_does_not_become_questionnaire_absence() {
    let fx = render_fixture("---\nquestionnaire: [\n---\n# Letter\n", false).await;
    assert!(matches!(
        workflows::notation_session::render_answer_projection(
            &fx.surreal,
            Some(&fx.storage),
            fx.notation_id
        )
        .await,
        Err(workflows::notation_session::NotationSessionError::Spec(_))
    ));
}

#[tokio::test]
async fn declared_bare_question_is_not_shadowed_by_qualified_answer_alias() {
    for bare_on_route in [false, true] {
        let snapshot = serde_json::json!({
            "spec": {"BEGIN": {"_": "custom_single_choice__path"},
                "custom_single_choice__path": {"a": "custom_text__a", "b": "custom_text"},
                "custom_text__a": {"_": if bare_on_route { "custom_text" } else { "END" }},
                "custom_text": {"_": "END"}, "END": {}},
            "prompts": {}, "choices": {"path": {"a": "A", "b": "B"}}
        });
        let fx = render_fixture_with_snapshot(
            "# Synthetic\n\nQUALIFIED: {{custom_text__a}}\n\n{{#if custom_text}}BARE: {{custom_text}}{{/if}}\n",
            Some(snapshot), "alias_fixture").await;
        let notation = store::notations::find_by_id(&fx.surreal, fx.notation_id)
            .await
            .unwrap()
            .unwrap();
        // Record the bare row first so a later qualified row cannot overwrite its authority.
        for (state, value) in [
            ("custom_text", "BareValue"),
            ("custom_single_choice__path", "a"),
            ("custom_text__a", "QualifiedValue"),
        ] {
            let code = state.split_once("__").map_or(state, |(code, _)| code);
            let question = store::questions::find_by_code(&fx.surreal, code)
                .await
                .unwrap()
                .unwrap();
            store::answers::record(
                &fx.surreal,
                &store::answers::NewAnswer::new(
                    question.id,
                    notation.person_id,
                    store::answers::primitive(value),
                )
                .in_notation(fx.notation_id, state),
            )
            .await
            .unwrap();
        }
        let before = store::answers::for_notation(&fx.surreal, fx.notation_id)
            .await
            .unwrap();
        let html = reviewed_html(&fx).await;
        assert!(html.contains("QUALIFIED: QualifiedValue"));
        if bare_on_route {
            assert!(html.contains("BARE: BareValue"));
        } else {
            assert!(
                !html.contains("BARE:"),
                "off-route bare state recreated by alias"
            );
        }
        assert_eq!(
            store::answers::for_notation(&fx.surreal, fx.notation_id)
                .await
                .unwrap()
                .len(),
            before.len()
        );
    }
}

async fn post_native(
    fx: &Fixture,
    action: &str,
    fields: &[(&str, &str)],
) -> axum::response::Response {
    fx.app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/app/lawyer/notations/{}/{action}", fx.notation_id))
                .header("authorization", &fx.acting)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(
                    url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(fields.iter().copied())
                        .finish(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn native_reask_refuses_off_route_flags_and_changed_chains_without_writes() {
    use http_body_util::BodyExt;
    let fx = branch_review_fixture().await;
    let response = post_native(&fx, "request-changes", &[("q:custom_text__b", "on")]).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(state_of(&fx).await, "lawyer_review");
    assert!(store::reask::flagged_questions(&fx.surreal, fx.notation_id)
        .await
        .unwrap()
        .is_empty());
    let response = post_native(
        &fx,
        "request-changes",
        &[
            ("q:custom_text__a", "on"),
            ("q:custom_single_choice__path", "on"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let before = store::answers::for_notation(&fx.surreal, fx.notation_id)
        .await
        .unwrap();
    let response = post_native(
        &fx,
        "reask",
        &[
            ("a:custom_text__a", "Must not write"),
            ("a:custom_single_choice__path", "b"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("fresh intake"));
    assert_eq!(
        store::answers::for_notation(&fx.surreal, fx.notation_id)
            .await
            .unwrap(),
        before
    );
    assert_eq!(state_of(&fx).await, "reask__client");
    let response = post_native(
        &fx,
        "reask",
        &[
            ("a:custom_text__a", "Corrected"),
            ("a:custom_single_choice__path", "c"),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(state_of(&fx).await, "lawyer_review");
    let stale = stale_flag_fixture().await;
    let before = store::answers::for_notation(&stale.surreal, stale.notation_id)
        .await
        .unwrap();
    let response = post_native(&stale, "reask", &[("a:custom_text__b", "Must not replace")]).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        store::answers::for_notation(&stale.surreal, stale.notation_id)
            .await
            .unwrap(),
        before
    );
}

#[tokio::test]
async fn identifiable_stripped_seed_body_without_snapshot_requires_fresh_intake() {
    use http_body_util::BodyExt;
    let fx = stripped_seed_fixture().await;
    assert!(
        workflows::notation_session::render_answer_projection(
            &fx.surreal,
            Some(&fx.storage),
            fx.notation_id
        )
        .await
        .is_err(),
        "a seed's stripped frontmatter does not prove questionnaire absence"
    );
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/app/lawyer/notations/{}/review", fx.notation_id))
                .header("authorization", &fx.acting)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("fresh intake"));
    let response = post_json(
        &fx,
        "request-changes",
        Some(&fx.acting),
        serde_json::json!({"flagged": ["person__client"]}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "questionnaire_authority_missing");
    assert!(store::reask::flagged_questions(&fx.surreal, fx.notation_id)
        .await
        .unwrap()
        .is_empty());
}

async fn assert_review_refused(fx: &Fixture, status: StatusCode) {
    use http_body_util::BodyExt;
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/app/lawyer/notations/{}/review", fx.notation_id))
                .header("authorization", &fx.acting)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), status);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&body);
    assert!(!body.contains("BOUND_CLIENT:"));
    assert!(!body.contains("libra@example.com"));
}

async fn stripped_seed_fixture() -> Fixture {
    let mut fx = build_fixture().await;
    let original = store::notations::find_by_id(&fx.surreal, fx.notation_id)
        .await
        .unwrap()
        .unwrap();
    fx.notation_id = store::notations::create(
        &fx.surreal,
        &store::notations::NewNotation::new(
            original.template_id,
            original.person_id,
            original.project_id,
            "lawyer_review",
        ),
    )
    .await
    .unwrap()
    .id;
    fx
}

#[tokio::test]
async fn stripped_seed_reask_refuses_native_and_rest_writes_without_recovering_catalog_route() {
    use http_body_util::BodyExt;
    let fx = stripped_seed_fixture().await;
    let response = post_native(&fx, "request-changes", &[("q:person__client", "on")]).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    store::notations::update_state(&fx.surreal, fx.notation_id, "reask__client")
        .await
        .unwrap();
    let person = store::notations::find_by_id(&fx.surreal, fx.notation_id)
        .await
        .unwrap()
        .unwrap()
        .person_id;
    store::reask::record_change_request(
        &fx.surreal,
        fx.notation_id,
        person,
        &["person__client".into()],
        None,
    )
    .await
    .unwrap();
    let before = store::answers::for_notation(&fx.surreal, fx.notation_id)
        .await
        .unwrap();
    let response = post_json(
        &fx,
        "reask",
        Some(&fx.acting),
        serde_json::json!({"answers": {"person__client": "Must not write"}}),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "questionnaire_authority_missing");
    let response = post_native(&fx, "reask", &[("a:person__client", "Must not write")]).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/app/lawyer/notations/{}/reask", fx.notation_id))
                .header("authorization", &fx.acting)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("fresh intake"));
    assert_eq!(
        store::answers::for_notation(&fx.surreal, fx.notation_id)
            .await
            .unwrap(),
        before
    );
}
