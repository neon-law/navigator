//! The real command treats locally cached roles as informational.

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn tampered_cached_owner_role_cannot_authorize_reference_acquisition() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/app/api/ephemeris/sources"))
        .and(header("authorization", "Bearer synthetic-lower-role-token"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    let parent = tempfile::tempdir().unwrap();
    let credentials = parent.path().join("credentials.json");
    std::fs::write(
        &credentials,
        serde_json::to_vec(&serde_json::json!({"hosts": {server.uri(): {
            "token": "synthetic-lower-role-token", "role": "owner", "expires_at": i64::MAX
        }}}))
        .unwrap(),
    )
    .unwrap();
    let destination = parent.path().join("reference");
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_navigator"))
        .args(["ephemeris", "download", "--host", &server.uri(), "--dir"])
        .arg(&destination)
        .env("NAVIGATOR_CREDENTIALS_FILE", &credentials)
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("HTTP 403"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-lower-role-token"));
    assert!(!destination.exists());
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
