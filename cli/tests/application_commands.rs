use clap::error::ErrorKind;

#[test]
fn application_commands_are_in_navigator_without_operator_access() {
    let command = cli::navigator_command();
    for args in [
        vec!["navigator", "mail", "--help"],
        vec!["navigator", "inbox", "--help"],
        vec!["navigator", "downloads", "--help"],
        vec!["navigator", "legal-plans", "--help"],
        vec!["navigator", "notation", "preview", "--help"],
        vec!["navigator", "site", "person", "create", "--help"],
    ] {
        let error = command.clone().try_get_matches_from(args).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DisplayHelp, "{error}");
    }
    for verb in ["ops", "dev"] {
        assert!(command
            .clone()
            .try_get_matches_from(["navigator", verb])
            .is_err());
    }
}

#[test]
fn person_creation_accepts_contacts_without_project_scope() {
    let command = cli::navigator_command();
    let base = [
        "navigator",
        "site",
        "person",
        "create",
        "--name",
        "Sample Client",
        "--email",
        "client@example.com",
    ];
    assert!(command.clone().try_get_matches_from(base).is_ok());
    for scope in [
        ["--project", "sample-litigation"],
        ["--firm", "Sample Firm"],
    ] {
        assert!(command
            .clone()
            .try_get_matches_from(base.into_iter().chain(scope))
            .is_ok());
    }
    assert!(command
        .try_get_matches_from(base.into_iter().chain([
            "--project",
            "sample-litigation",
            "--firm",
            "Sample Firm"
        ]))
        .is_err());
}

#[test]
fn preview_renders_an_uncommitted_draft_without_a_site_session() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("draft.md");
    let output = dir.path().join("review.pdf");
    std::fs::write(&source, "---\nkind: will\ntitle: Draft Will\nrespondent_type: person\nconfidential: true\nquestionnaire:\n  BEGIN:\n    _: END\n  END: {}\nworkflow:\n  BEGIN:\n    intake_submitted: lawyer_review\n  lawyer_review:\n    approved: END\n    rejected: END\n  END: {}\n---\n\n# Draft Will\n\n## I. Disposition\n\nI give my property to {{beneficiary}}.\n").unwrap();
    let original = std::fs::read(&source).unwrap();
    let output_status = std::process::Command::new(assert_cmd::cargo::cargo_bin!("navigator"))
        .current_dir(dir.path())
        .args(["notation", "preview"])
        .arg(&source)
        .arg("--out")
        .arg(&output)
        .arg("--no-open")
        .output()
        .unwrap();
    assert!(
        output_status.status.success(),
        "{}",
        format!(
            "{}{}",
            String::from_utf8_lossy(&output_status.stderr),
            String::from_utf8_lossy(&output_status.stdout)
        )
    );
    assert!(std::fs::read(output).unwrap().starts_with(b"%PDF-"));
    assert_eq!(std::fs::read(source).unwrap(), original);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[tokio::test]
async fn contacts_use_the_navigator_session_without_a_project_lookup() {
    use wiremock::{
        matchers::{body_partial_json, header, method, path},
        Mock, MockServer, ResponseTemplate,
    };
    let server = MockServer::start().await;
    let person = uuid::Uuid::now_v7();
    Mock::given(method("POST"))
        .and(path("/app/api/people"))
        .and(header("authorization", "Bearer navigator-session"))
        .and(body_partial_json(serde_json::json!({"name":"Sample Contact", "email":"contact@example.com", "role":"client", "project_id":null})))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({"id":person})))
        .expect(1).mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let credentials = dir.path().join("credentials.json");
    std::fs::write(&credentials, serde_json::to_vec(&serde_json::json!({
        "hosts": { server.uri(): {"token":"navigator-session", "role":"lawyer", "expires_at":4_102_444_800_i64} }
    })).unwrap()).unwrap();
    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("navigator"));
    command
        .current_dir(dir.path())
        .args([
            "site",
            "person",
            "create",
            "--name",
            "Sample Contact",
            "--email",
            "contact@example.com",
        ])
        .env("NAVIGATOR_CREDENTIALS_FILE", credentials);
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        person.to_string()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}
