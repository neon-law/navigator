//! Exercise the authenticated Navigator route against a local `DocuSign` HTTP server.
use super::{json, Fixture};
use axum::http::StatusCode;
use portal::signature::{
    CompletedDocuments, DocuSignSignatureProvider, RecipientView, SendMode, SignatureError,
    SignatureManifest, SignatureProvider, SignatureRequestId,
};
use serde_json::json;
use std::sync::Arc;
use store::{persons::Role, surreal::SurrealDb};
use uuid::Uuid;
use wiremock::{
    matchers::{header, method, path},
    Mock, MockServer, ResponseTemplate,
};

/// Stand in for the Firm's credential resolver. The resolved adapter uses real HTTP.
struct FixtureConnection(DocuSignSignatureProvider);

#[async_trait::async_trait]
impl SignatureProvider for FixtureConnection {
    async fn for_project(
        &self,
        _: &SurrealDb,
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
