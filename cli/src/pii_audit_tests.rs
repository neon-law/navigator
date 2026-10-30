// LAW-162: the pre-filing issue scan refuses a draft that carries a Project's
// code or any of its attached people's identifiers, resolved from the store.

use store::persons::NewPerson;
use store::surreal::SurrealDb;

async fn cruller_fixture() -> (SurrealDb, store::projects::Project) {
    let db = store::surreal::test_support::mem().await;
    let project = store::test_support::seed_project(&db, "Cruller Estate").await;
    let person = store::persons::find_or_create(
        &db,
        &NewPerson {
            phone: Some("702-555-0142".to_string()),
            ..NewPerson::new("Harriet Cruller", "harriet@crullers.example")
        },
    )
    .await
    .expect("seed the client person");
    store::projects::add_participation(&db, project.id, person.id, "client")
        .await
        .expect("attach the client to the project");
    (db, project)
}

#[tokio::test]
async fn issue_scan_terms_collect_project_and_participant_identifiers() {
    let (db, project) = cruller_fixture().await;

    let terms = crate::pii_audit::private_terms(&db, &[project.code.clone()])
        .await
        .expect("resolve private terms");

    for known in [
        project.code.clone(),
        "Cruller Estate".to_string(),
        "Harriet Cruller".to_string(),
        "harriet@crullers.example".to_string(),
        "702-555-0142".to_string(),
    ] {
        assert!(
            terms.iter().any(|t| t == &known),
            "terms lack {known:?}: {terms:?}"
        );
    }

    // The resolved terms feed the existing scanner: a draft naming the client
    // is refused, and the scanner stays silent on the abstract draft.
    let draft = "Harriet Cruller asked where the filing stands";
    assert!(!crate::feedback_scan::scan(draft, &terms).is_empty());
    assert!(crate::feedback_scan::scan(
        "the status command exits 0 when the folder is empty",
        &terms
    )
    .is_empty());
}

#[tokio::test]
async fn issue_scan_terms_refuse_an_unknown_project_code() {
    let db = store::surreal::test_support::mem().await;

    let resolved = crate::pii_audit::private_terms(&db, &["no-such-project".to_string()]).await;

    assert!(resolved.is_err(), "a typo'd code must not scan as nothing");
}

#[tokio::test]
async fn issue_scan_terms_skip_participants_of_other_projects() {
    let (db, project) = cruller_fixture().await;
    let other = store::test_support::seed_project(&db, "Unrelated Matter").await;

    let terms = crate::pii_audit::private_terms(&db, &[other.code.clone()])
        .await
        .expect("resolve private terms");

    assert!(terms.iter().any(|t| t == "Unrelated Matter"));
    assert!(!terms.iter().any(|t| t == "Harriet Cruller"));
    assert!(
        crate::pii_audit::private_terms(&db, &[project.code, other.code])
            .await
            .expect("resolve both projects")
            .iter()
            .any(|t| t == "Harriet Cruller")
    );
}

#[test]
fn merged_terms_dedupe_case_insensitively_and_skip_blanks() {
    let resolved = vec!["sample-estate".to_string(), "Harriet Cruller".to_string()];
    let manual = vec![
        "  ".to_string(),
        "SAMPLE-ESTATE".to_string(),
        "harriet cruller".to_string(),
        "Opposing Counsel".to_string(),
    ];

    let merged = crate::pii_audit::merge_terms(&resolved, &manual);

    assert_eq!(
        merged,
        vec![
            "sample-estate".to_string(),
            "Harriet Cruller".to_string(),
            "Opposing Counsel".to_string()
        ]
    );
}

#[tokio::test]
async fn issue_scan_terms_error_when_a_project_code_is_blank() {
    let db = store::surreal::test_support::mem().await;

    let resolved = crate::pii_audit::private_terms(&db, &["  ".to_string()]).await;

    assert!(resolved.is_err());
}

use super::*;
use std::process::Command;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn repository() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("fixture directory");
    assert!(Command::new("git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .status()
        .unwrap()
        .success());
    std::fs::create_dir_all(temp.path().join(".github/pii")).unwrap();
    std::fs::write(temp.path().join(".github/pii/allowlist.toml"), "").unwrap();
    temp
}

fn track(root: &Path, path: &str, text: &str) {
    std::fs::write(root.join(path), text).unwrap();
    assert!(Command::new("git")
        .args(["add", "--", path])
        .current_dir(root)
        .status()
        .unwrap()
        .success());
}

async fn analyzer(body: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/supportedentities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            "PERSON",
            "EMAIL_ADDRESS",
            "PHONE_NUMBER",
            "US_SSN",
            "US_ITIN",
            "CREDIT_CARD",
            "IBAN_CODE"
        ])))
        .with_priority(5)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .and(body_partial_json(serde_json::json!({"text":NAME_CANARY})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"entity_type":"PERSON", "start":11, "end":21, "score":0.85}
        ])))
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .with_priority(10)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn findings_keep_original_line_and_never_retain_detected_values() {
    let repo = repository();
    let person = ["Zelda", "Quillstone"].join(" ");
    track(
        repo.path(),
        "sample.md",
        &format!("# Sample\n\nContact {person}.\n"),
    );
    let server = analyzer(serde_json::json!([
        {"entity_type":"PERSON", "start":8, "end":24, "score":0.85}
    ]))
    .await;
    let found = audit_tree(repo.path(), &server.uri()).await.unwrap();
    assert_eq!(
        found,
        vec![Finding {
            path: "sample.md".into(),
            line: 3,
            entity: "PERSON".into()
        }]
    );
    assert!(!format!("{found:?}").contains(&person));
}

#[tokio::test]
async fn unavailable_analyzer_fails_closed() {
    let repo = repository();
    track(repo.path(), "sample.md", "# Example\n");
    let server = MockServer::start().await;
    let result = audit_tree(repo.path(), &server.uri()).await;
    assert!(
        result.is_err(),
        "an unavailable recognizer must not pass CI"
    );
}

#[tokio::test]
async fn refuses_remote_analyzers_before_sending_repository_text() {
    let repo = repository();
    assert!(audit_tree(repo.path(), "https://scanner.example")
        .await
        .is_err());
}

#[tokio::test]
async fn an_advertised_person_recognizer_without_a_working_model_fails() {
    let repo = repository();
    let server = analyzer(serde_json::json!([])).await;
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .with_priority(1)
        .mount(&server)
        .await;
    assert!(audit_tree(repo.path(), &server.uri()).await.is_err());
}

#[tokio::test]
async fn unicode_offsets_preserve_the_original_line() {
    let repo = repository();
    track(
        repo.path(),
        "sample.md",
        "# Fixture\n\nÉlodie — Zelda Quillstone\n",
    );
    let server = analyzer(serde_json::json!([
        {"entity_type":"PERSON", "start":9, "end":25, "score":0.85}
    ]))
    .await;
    let found = audit_tree(repo.path(), &server.uri()).await.unwrap();
    assert_eq!(found[0].line, 3);
    assert_eq!(found[0].entity, "PERSON");
}

#[tokio::test]
async fn reserved_email_and_reviewed_name_exceptions_remain_supported() {
    let repo = repository();
    let email = "sample@contact.example";
    track(repo.path(), "sample.md", email);
    let server = analyzer(serde_json::json!([
        {"entity_type":"EMAIL_ADDRESS", "start":0, "end":email.len(), "score":1.0}
    ]))
    .await;
    assert!(audit_tree(repo.path(), &server.uri())
        .await
        .unwrap()
        .is_empty());
    std::fs::write(
        repo.path().join(".github/pii/allowlist.toml"),
        "[[names]]\nreason = 'Synthetic fixture'\nvalues = ['Zelda Quillstone']\n",
    )
    .unwrap();
    track(repo.path(), "sample.md", "Zelda Quillstone");
    let server = analyzer(serde_json::json!([
        {"entity_type":"PERSON", "start":0, "end":16, "score":0.85}
    ]))
    .await;
    assert!(audit_tree(repo.path(), &server.uri())
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn malformed_offsets_and_analyzer_error_bodies_never_echo_input() {
    let repo = repository();
    let value = ["Zelda", "Quillstone"].join(" ");
    track(repo.path(), "sample.md", &value);
    let server = analyzer(serde_json::json!([
        {"entity_type":"PERSON", "start":0, "end":100_000, "score":0.85}
    ]))
    .await;
    let error = audit_tree(repo.path(), &server.uri()).await.unwrap_err();
    assert!(!format!("{error:#}").contains(&value));
    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(ResponseTemplate::new(500).set_body_string(value.clone()))
        .with_priority(1)
        .mount(&server)
        .await;
    let error = audit_tree(repo.path(), &server.uri()).await.unwrap_err();
    assert!(!format!("{error:#}").contains(&value));
}

#[tokio::test]
async fn missing_categories_and_redirects_fail_closed() {
    let repo = repository();
    let server = analyzer(serde_json::json!([])).await;
    Mock::given(method("GET"))
        .and(path("/supportedentities"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!(["PERSON"])))
        .with_priority(1)
        .mount(&server)
        .await;
    assert!(audit_tree(repo.path(), &server.uri()).await.is_err());
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/supportedentities"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", "https://scanner.example"),
        )
        .mount(&server)
        .await;
    assert!(audit_tree(repo.path(), &server.uri()).await.is_err());
}

#[tokio::test]
async fn empty_exception_reasons_are_refused_before_network_access() {
    let repo = repository();
    std::fs::write(
        repo.path().join(".github/pii/allowlist.toml"),
        "[[names]]\nreason = ''\nvalues = ['Fixture Person']\n",
    )
    .unwrap();
    assert!(audit_tree(repo.path(), "http://127.0.0.1:1").await.is_err());
}

#[test]
fn required_ci_gate_runs_pii_with_pinned_local_analyzer() {
    let workflow = include_str!("../../.github/workflows/ci.yml");
    let gate = workflow
        .split("  gate:\n")
        .nth(1)
        .unwrap()
        .split("\n  rust:")
        .next()
        .unwrap();
    assert!(gate.contains("presidio-analyzer:2.2.364@sha256:"));
    assert!(gate.contains("options: --log-driver none"));
    assert!(gate.contains("pii --analyzer-url http://127.0.0.1:3000"));
    assert!(gate.contains("--retry-connrefused"));
    assert!(!gate.contains("continue-on-error: true"));
    assert!(!workflow.contains(".github/pii/scan.py"));
}

#[test]
fn reviewed_public_captions_match_normalized_initials() {
    let allow = Allowlist::parse(
        "[[names]]\nreason = 'Published corporate case caption'\nvalues = ['D.R. Horton']\n",
    )
    .unwrap();
    assert!(allow.allows("PERSON", "D.R. Horton", &Patterns::new().unwrap()));
    assert!(!allow.allows("PERSON", "D.R. Unlisted", &Patterns::new().unwrap()));
}

#[test]
fn firm_email_exemption_uses_the_checked_in_allowlist_and_domain_boundaries() {
    let allow = Allowlist::parse(include_str!("../../.github/pii/allowlist.toml")).unwrap();
    let patterns = Patterns::new().unwrap();
    for domain in ["neonlaw.com", "NEONLAW.COM", "mail.neonlaw.com"] {
        let email = format!("synthetic-fixture@{domain}");
        assert!(allow.allows("EMAIL_ADDRESS", &email, &patterns));
    }
    for domain in ["notneonlaw.com", "neonlaw.com.other-domain.net"] {
        let email = format!("synthetic-fixture@{domain}");
        assert!(!allow.allows("EMAIL_ADDRESS", &email, &patterns));
    }
}

#[test]
fn the_public_aster_introduction_does_not_allow_unrelated_names() {
    let allow = Allowlist::parse(include_str!("../../.github/pii/allowlist.toml")).unwrap();
    let patterns = Patterns::new().unwrap();
    assert!(allow.allows("PERSON", "Meet Aster", &patterns));
    assert!(!allow.allows("PERSON", "Meet Zelda", &patterns)); // pii-scan: allow — synthetic negative-test name
    assert!(!allow.allows("PERSON", "Aster Quillstone", &patterns)); // pii-scan: allow — synthetic negative-test name
}
