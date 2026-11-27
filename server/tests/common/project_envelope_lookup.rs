//! Exercise the authenticated Navigator route against a local `DocuSign` HTTP server.
use super::{json, Fixture};
use axum::http::StatusCode;
use portal::signature::{
    CompletedDocuments, DocuSignSignatureProvider, RecipientView, SendMode, SignatureError,
    SignatureManifest, SignatureProvider, SignatureRequestId,
};
use serde_json::json;
use std::sync::Arc;
use store::{application_store::ApplicationStore, persons::Role};
use uuid::Uuid;
use wiremock::{
    matchers::{body_json, header, method, path},
    Mock, MockServer, ResponseTemplate,
};

/// Stand in for the Firm's credential resolver. The resolved adapter uses real HTTP.
struct FixtureConnection(DocuSignSignatureProvider);

#[async_trait::async_trait]
impl SignatureProvider for FixtureConnection {
    async fn for_project(
        &self,
        _: &ApplicationStore,
        _: Uuid,
    ) -> Result<Option<DocuSignSignatureProvider>, SignatureError> {
        Ok(Some(self.0.clone()))
    }
    async fn for_firm(
        &self,
        _: &ApplicationStore,
        _: Uuid,
    ) -> Result<Option<DocuSignSignatureProvider>, SignatureError> {
        Ok(Some(self.0.clone()))
    }
    async fn send_for_signature(
        &self,
        _: Uuid,
        _: &[u8],
        _: &SignatureManifest,
        _: SendMode,
    ) -> Result<SignatureRequestId, SignatureError> {
        panic!("a lookup must not prepare or send")
    }
    async fn dispatch_prepared(&self, _: &SignatureRequestId) -> Result<(), SignatureError> {
        panic!("a lookup must not dispatch a draft")
    }
    async fn create_recipient_view(
        &self,
        _: &SignatureRequestId,
        _: &RecipientView,
    ) -> Result<String, SignatureError> {
        panic!("a lookup must not start a signing ceremony")
    }
    async fn create_sender_view(
        &self,
        _: &SignatureRequestId,
        _: &str,
    ) -> Result<String, SignatureError> {
        panic!("a lookup must not start a sender session")
    }
    async fn fetch_completed_documents(
        &self,
        _: &SignatureRequestId,
    ) -> Result<CompletedDocuments, SignatureError> {
        panic!("a lookup must not file documents")
    }
}

#[tokio::test]
async fn reads_an_unregistered_demo_draft_only_on_the_signers_project() {
    let http = MockServer::start().await;
    let account = Uuid::now_v7();
    let envelope = Uuid::now_v7();
    let provider = DocuSignSignatureProvider::new(
        http.uri(),
        account.to_string(),
        "local-test",
        "firm@example.com",
        "Test Firm",
    );
    let Some(fx) = Fixture::new(Some(Arc::new(FixtureConnection(provider)))).await else {
        return;
    };
    let route = format!("/app/api/projects/{}/envelopes/{envelope}", fx.project);
    assert_eq!(
        fx.get(&route, Some(Role::Lawyer), Some(fx.lawyer))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert!(http.received_requests().await.unwrap().is_empty());
    fx.participate().await;
    let pool = fx.pg.pool();
    let firm = Uuid::now_v7();
    let signer = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO firm(id,name,slug,status) VALUES($1,'Test Firm','test-firm','active')",
    )
    .bind(firm)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE project SET firm_id=$1 WHERE id=$2")
        .bind(firm)
        .bind(fx.project)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO person(id,name,email,role) VALUES($1,'Test Signer','client@example.com','client')").bind(signer).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO person_project_role(person_id,project_id,participation,is_client_dri) VALUES($1,$2,'client',true)").bind(signer).bind(fx.project).execute(pool).await.unwrap();

    let base = format!("/v2.1/accounts/{account}/envelopes/{envelope}");
    let documents = json!([{"name":"synthetic-onboarding__letter.pdf"}]);
    for (suffix, body) in [
        (
            "",
            json!({"envelopeId":envelope,"status":"created","envelopeDocuments":documents}),
        ),
        (
            "/recipients",
            json!({"signers":[{"name":"Test Signer","email":"client@example.com","status":"created","routingOrder":"1"}]}),
        ),
        ("/custom_fields", json!({"textCustomFields":[]})),
        ("/documents", json!({"envelopeDocuments":documents})),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{base}{suffix}")))
            .and(header("authorization", "Bearer local-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(3)
            .mount(&http)
            .await;
    }
    for _ in 0..2 {
        let response = fx.get(&route, Some(Role::Lawyer), Some(fx.lawyer)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = json(response).await;
        assert_eq!(body["envelopeId"], envelope.to_string());
        assert_eq!(body["status"], "created");
        assert_eq!(body["envelopeDocuments"], documents);
        assert_eq!(body["signers"][0]["routingOrder"], "1");
    }
    sqlx::query(
        "UPDATE person_project_role SET is_client_dri=false WHERE project_id=$1 AND person_id=$2",
    )
    .bind(fx.project)
    .bind(signer)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        fx.get(&route, Some(Role::Lawyer), Some(fx.lawyer))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let writes: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM signature)+(SELECT count(*) FROM project_envelope)+(SELECT count(*) FROM asset)+(SELECT count(*) FROM notation)")
        .fetch_one(pool).await.unwrap();
    assert_eq!(writes, 0);
    assert_eq!(http.received_requests().await.unwrap().len(), 12);
}

/// LAW-501: a Firm lawyer reads an envelope no Project records — one sent
/// from the DocuSign web app — by its ID, and learns which Project it
/// matches or why none, with nothing written.
#[tokio::test]
#[allow(clippy::too_many_lines)] // one Firm fixture walks every read and void outcome
async fn a_firm_lawyer_reads_an_unrecorded_envelope_by_id_and_learns_its_match() {
    let http = MockServer::start().await;
    let account = Uuid::now_v7();
    let envelope = Uuid::now_v7();
    let unknown = Uuid::now_v7();
    let provider = DocuSignSignatureProvider::new(
        http.uri(),
        account.to_string(),
        "local-test",
        "firm@example.com",
        "Test Firm",
    );
    let Some(fx) = Fixture::new(Some(Arc::new(FixtureConnection(provider)))).await else {
        return;
    };
    let route = format!("/app/api/envelopes/{envelope}");
    let as_lawyer = || fx.get(&route, Some(Role::Lawyer), Some(fx.lawyer));

    // No Firm membership: no account to read, and DocuSign is never called.
    let response = as_lawyer().await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json(response).await["error"], "docusign_not_configured");
    assert!(http.received_requests().await.unwrap().is_empty());

    let pool = fx.pg.pool();
    let firm = Uuid::now_v7();
    let signer = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO firm(id,name,slug,status) VALUES($1,'Test Firm','test-firm','active')",
    )
    .bind(firm)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE project SET firm_id=$1 WHERE id=$2")
        .bind(firm)
        .bind(fx.project)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO person_firm_role(person_id,firm_id,membership) VALUES($1,$2,'lawyer')",
    )
    .bind(fx.lawyer)
    .bind(firm)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO person(id,name,email,role) VALUES($1,'Test Signer','client@example.com','client')").bind(signer).execute(pool).await.unwrap();
    sqlx::query("INSERT INTO person_project_role(person_id,project_id,participation,is_client_dri) VALUES($1,$2,'client',true)").bind(signer).bind(fx.project).execute(pool).await.unwrap();

    let base = format!("/v2.1/accounts/{account}/envelopes/{envelope}");
    for (suffix, body) in [
        ("", json!({"envelopeId":envelope,"status":"completed"})),
        (
            "/recipients",
            json!({"signers":[{"name":"Test Signer","email":"client@example.com","status":"completed","routingOrder":"1"}]}),
        ),
        ("/custom_fields", json!({"textCustomFields":[]})),
        (
            "/documents",
            json!({"envelopeDocuments":[{"name":"substitution.pdf"}]}),
        ),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{base}{suffix}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&http)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!(
            "/v2.1/accounts/{account}/envelopes/{unknown}"
        )))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"errorCode":"ENVELOPE_DOES_NOT_EXIST"})),
        )
        .mount(&http)
        .await;

    // A Firm lawyer who is not on the matching Project reads the envelope
    // but not which Project it belongs to.
    let response = as_lawyer().await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["status"], "completed");
    assert_eq!(body["signers"][0]["status"], "completed");
    assert_eq!(body["navigator"]["recorded"], false);
    assert_eq!(body["navigator"]["project"], serde_json::Value::Null);
    assert_eq!(body["navigator"]["reason"], "project_not_visible");

    fx.participate().await;
    let body = json(as_lawyer().await).await;
    assert_eq!(body["navigator"]["project"], fx_code(&fx).await);
    assert_eq!(body["navigator"]["reason"], serde_json::Value::Null);

    // The client is no longer the designated signer: the reason says so.
    sqlx::query(
        "UPDATE person_project_role SET is_client_dri=false WHERE project_id=$1 AND person_id=$2",
    )
    .bind(fx.project)
    .bind(signer)
    .execute(pool)
    .await
    .unwrap();
    let body = json(as_lawyer().await).await;
    assert_eq!(body["navigator"]["project"], serde_json::Value::Null);
    assert_eq!(body["navigator"]["reason"], "no_project_match");

    // An ID outside the Firm's account answers with DocuSign's reason.
    let response = fx
        .get(
            &format!("/app/api/envelopes/{unknown}"),
            Some(Role::Lawyer),
            Some(fx.lawyer),
        )
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = json(response).await;
    assert_eq!(body["error"], "envelope_not_found");
    assert!(body["message"].as_str().unwrap().contains("404"), "{body}");

    // A client never reaches the read.
    let response = fx.get(&route, Some(Role::Client), Some(signer)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // LAW-430: void refuses a completed envelope and an empty reason, voids
    // one out for signature, and deletes a held draft.
    let void = |id: Uuid, reason: &str| {
        post_json(
            &fx,
            format!("/app/api/envelopes/{id}/void"),
            json!({"reason": reason}),
        )
    };
    let response = void(envelope, "Declined").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(json(response).await["error"], "envelope_closed");
    assert_eq!(
        void(envelope, "  ").await.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let (sent, draft) = (Uuid::now_v7(), Uuid::now_v7());
    for (id, status) in [(sent, "sent"), (draft, "created")] {
        let base = format!("/v2.1/accounts/{account}/envelopes/{id}");
        Mock::given(method("GET"))
            .and(path(base.clone()))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"envelopeId":id,"status":status})),
            )
            .mount(&http)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("{base}/recipients")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"signers":[]})))
            .mount(&http)
            .await;
    }
    Mock::given(method("PUT"))
        .and(path(format!("/v2.1/accounts/{account}/envelopes/{sent}")))
        .and(body_json(
            json!({"status":"voided","voidedReason":"Pitch declined"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"envelopeId":sent})))
        .expect(1)
        .mount(&http)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("/v2.1/accounts/{account}/folders/recyclebin")))
        .and(body_json(json!({"envelopeIds":[draft]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&http)
        .await;
    let response = void(sent, "Pitch declined").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["status"], "voided");
    let response = void(draft, "Stale render").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["status"], "deleted");

    let writes: i64 = sqlx::query_scalar("SELECT (SELECT count(*) FROM signature)+(SELECT count(*) FROM project_envelope)+(SELECT count(*) FROM asset)+(SELECT count(*) FROM notation)")
        .fetch_one(pool).await.unwrap();
    assert_eq!(writes, 0);
}

async fn post_json(
    fx: &Fixture,
    route: String,
    body: serde_json::Value,
) -> axum::response::Response {
    use tower::ServiceExt as _;
    let mut session = portal::session::SessionData::fresh("synthetic", Role::Lawyer);
    session.person_id = Some(fx.lawyer);
    let token = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY).encode(&session);
    let request = axum::http::Request::post(route)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .unwrap();
    store::postgres::PostgresDb::with_test_db(fx.pg.clone(), fx.app.clone().oneshot(request))
        .await
        .unwrap()
}

async fn fx_code(fx: &Fixture) -> String {
    sqlx::query_scalar("SELECT code FROM project WHERE id=$1")
        .bind(fx.project)
        .fetch_one(fx.pg.pool())
        .await
        .unwrap()
}
