//! The shipped CLI reads signatures with its Navigator session, without database credentials.
use std::process::Command;
use wiremock::{
    matchers::{header, method, path, query_param},
    Mock, MockServer, ResponseTemplate,
};

#[tokio::test]
async fn draft_signature_prefers_the_authenticated_api_and_announces_fallbacks() {
    for (status, fallback, expected) in [
        (200, true, "signature: source=runtime"),
        (404, true, "signature: source=env"),
        (503, true, "signature: source=env"),
        (
            404,
            false,
            "signing with the Gmail account's default signature",
        ),
    ] {
        let server = MockServer::start().await;
        let person = uuid::Uuid::now_v7();
        Mock::given(method("GET"))
            .and(path("/app/api/mail-signature"))
            .and(header("authorization", "Bearer sample-session"))
            .and(query_param("account", "attorney@example.com"))
            .respond_with(ResponseTemplate::new(status).set_body_json(serde_json::json!({
                "person_id":person, "body":"Sample Attorney", "updated_at":"2026-11-17T12:00:00Z"
            }))).expect(1).mount(&server).await;
        let dir = tempfile::tempdir().unwrap();
        let credentials = dir.path().join("credentials.json");
        let body = dir.path().join("body.txt");
        std::fs::write(&body, "Please review.").unwrap();
        std::fs::write(
            &credentials,
            serde_json::to_vec(&serde_json::json!({
                "hosts": { server.uri(): { "token":"sample-session", "expires_at":4_102_444_800_i64 } }
            }))
            .unwrap(),
        )
        .unwrap();
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("navigator"));
        command
            .current_dir(dir.path())
            .args(["mail", "draft", "--body-file"])
            .arg(body)
            .args(["--to", "client@example.com", "--subject", "Review"])
            .env("NAVIGATOR_CREDENTIALS_FILE", credentials)
            .env("NAVIGATOR_GMAIL_ACCOUNT", "attorney@example.com")
            .env("NAVIGATOR_GMAIL_CLIENT_ID", "sample-client")
            .env_remove("NAVIGATOR_SURREAL_URL")
            .env_remove("NAVIGATOR_POSTGRES_HOST")
            .env_remove("NAVIGATOR_GMAIL_SIGNATURE");
        if fallback {
            command.env("NAVIGATOR_GMAIL_SIGNATURE", "Fallback Attorney");
        }
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "status={status}: {stderr}");
        // No provider credentials are supplied, so this test cannot save a draft.
        assert!(!output.status.success());
        if status == 200 {
            assert!(stderr.contains(&person.to_string()));
            assert!(stderr.contains("2026-11-17T12:00:00Z"));
            assert!(!stderr.contains("source=env"));
        }
    }
}
