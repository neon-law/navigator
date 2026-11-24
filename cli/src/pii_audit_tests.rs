#[test]
fn merged_terms_dedupe_case_insensitively_and_skip_blanks() {
    let resolved = vec!["sample-estate".to_string(), "Harper Cruller".to_string()];
    let manual = vec![
        "  ".to_string(),
        "SAMPLE-ESTATE".to_string(),
        "harper cruller".to_string(),
        "Opposing Counsel".to_string(),
    ];

    let merged = crate::pii_audit::merge_terms(&resolved, &manual);

    assert_eq!(
        merged,
        vec![
            "sample-estate".to_string(),
            "Harper Cruller".to_string(),
            "Opposing Counsel".to_string()
        ]
    );
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
    track_existing(root, path);
}

fn track_existing(root: &Path, path: &str) {
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
fn the_analyzer_must_be_a_plain_loopback_origin() {
    for refused in [
        "not a url",
        "http://localhost:3000",
        "https://127.0.0.1:3000",
        "http://scanner@127.0.0.1:3000",
        "http://127.0.0.1:3000/analyze",
        "http://127.0.0.1:3000/?language=en",
        "http://127.0.0.1:3000/#fragment",
    ] {
        assert!(endpoint(refused).is_err(), "{refused}");
    }
    for local in ["http://127.0.0.1:3000", "http://[::1]:3000/"] {
        assert_eq!(endpoint(local).unwrap().path(), "/", "{local}");
    }
}

#[tokio::test]
async fn text_the_allowlist_or_the_file_type_excludes_never_reaches_the_analyzer() {
    let repo = repository();
    std::fs::write(
        repo.path().join(".github/pii/allowlist.toml"),
        "[[paths]]\nreason = 'Vendored code'\nvalues = ['third_party/*']\n",
    )
    .unwrap();
    std::fs::create_dir_all(repo.path().join("third_party")).unwrap();
    track(repo.path(), "third_party/credits.md", "Zelda Quillstone\n");
    track(repo.path(), "logo.svg", "Zelda Quillstone\n");
    track(
        repo.path(),
        "reviewed.md",
        "Zelda Quillstone <!-- pii-scan: allow -->\nshort\nno candidate on this line\n",
    );
    std::fs::write(repo.path().join("scan.bin.md"), [0xff, 0xfe, 0x40]).unwrap();
    track_existing(repo.path(), "scan.bin.md");
    let server = analyzer(serde_json::json!([
        {"entity_type":"PERSON", "start":0, "end":16, "score":0.85}
    ]))
    .await;

    let found = audit_tree(repo.path(), &server.uri()).await.unwrap();

    assert!(found.is_empty(), "{found:?}");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2, "only the recognizer checks are sent");
}

#[tokio::test]
async fn weak_or_implausible_detections_are_not_findings() {
    let repo = repository();
    track(repo.path(), "sample.md", "Zelda Quillstone wrote this.\n");
    let server = analyzer(serde_json::json!([
        {"entity_type":"PERSON", "start":0, "end":16, "score":0.2},
        {"entity_type":"PERSON", "start":0, "end":5, "score":0.9},
        {"entity_type":"US_SSN", "start":0, "end":5, "score":0.9},
        {"entity_type":"PHONE_NUMBER", "start":0, "end":5, "score":0.9},
        {"entity_type":"IBAN_CODE", "start":17, "end":22, "score":0.9}
    ]))
    .await;
    let found = audit_tree(repo.path(), &server.uri()).await.unwrap();
    assert_eq!(
        found,
        vec![Finding {
            path: "sample.md".into(),
            line: 1,
            entity: "IBAN_CODE".into()
        }]
    );
}

#[tokio::test]
async fn inputs_the_scan_cannot_bound_fail_closed() {
    let oversized = "@".repeat(MAX_LINE_CHARS + 1);
    let repo = repository();
    track(repo.path(), "sample.md", &oversized);
    let server = analyzer(serde_json::json!([])).await;
    let error = audit_tree(repo.path(), &server.uri()).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "PII input line exceeds the analyzer limit"
    );

    let repo = repository();
    std::os::unix::fs::symlink("sample.md", repo.path().join("link.md")).unwrap();
    track_existing(repo.path(), "link.md");
    let error = audit_tree(repo.path(), &server.uri()).await.unwrap_err();
    assert_eq!(error.to_string(), "PII input cannot be a symlink");

    let outside_git = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(outside_git.path().join(".github/pii")).unwrap();
    std::fs::write(outside_git.path().join(ALLOWLIST), "").unwrap();
    let error = audit_tree(outside_git.path(), &server.uri())
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "PII scan requires a Git checkout");

    Mock::given(method("POST"))
        .and(path("/analyze"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b' '; MAX_RESPONSE_BYTES + 1]))
        .with_priority(1)
        .mount(&server)
        .await;
    let error = audit_tree(repo.path(), &server.uri()).await.unwrap_err();
    assert_eq!(error.to_string(), "PII analyzer response exceeds its limit");
}

#[test]
fn only_plausible_values_of_each_category_count() {
    let patterns = Patterns::new().unwrap();
    for (entity, value, plausible) in [
        ("EMAIL_ADDRESS", "sample@contact.example", true),
        ("EMAIL_ADDRESS", "{user}@contact.example", false),
        ("EMAIL_ADDRESS", "git@contact.example/path", false),
        ("PHONE_NUMBER", "(702) 555-0100", true),
        ("PHONE_NUMBER", "+44 20 5550 0100", true), // pii-scan: allow — synthetic fixture
        ("PHONE_NUMBER", "2099-01-02", false),
        ("US_SSN", "000-00-0000", true), // pii-scan: allow — synthetic fixture
        ("US_ITIN", "000 00 0000", true), // pii-scan: allow — synthetic fixture
        ("US_SSN", "000000000", false),
        ("PERSON", "Zelda Quillstone", true),
        ("PERSON", "Zelda", false),
        ("PERSON", "zelda quillstone", false),
        ("CREDIT_CARD", "0000", true),
    ] {
        assert_eq!(patterns.plausible(entity, value), plausible, "{value}");
    }
}

#[test]
fn only_prose_strings_and_comments_of_a_source_line_are_scanned() {
    let patterns = Patterns::new().unwrap();
    for (path, line, text) in [
        (
            "docs/letter.md",
            "Dear Sample Client,",
            "Dear Sample Client,",
        ),
        ("NOTICE", "Copyright the authors", "Copyright the authors"),
        ("mark.png", "Sample Client", ""),
        (
            "src/lib.rs",
            r#"let name = "Sample Client"; // signed by counsel"#,
            "Sample Client  signed by counsel",
        ),
        (
            "src/lib.rs",
            r#""a wrapped string // with slashes""#,
            "a wrapped string // with slashes",
        ),
        ("schema.sql", "-- owner note", "owner note"),
        ("run.sh", "echo ok # owner note", "owner note"),
        ("src/lib.rs", "let count = 3;", ""),
    ] {
        assert_eq!(patterns.human_text(path, line), text, "{path}: {line}");
    }
}

#[test]
fn reviewed_exceptions_cover_paths_values_and_names() {
    let patterns = Patterns::new().unwrap();
    let allow = Allowlist::parse(concat!(
        "[[paths]]\nreason = 'Vendored'\nvalues = ['vendor/*', 'notice', 'fixtures/?.json']\n",
        "[[values]]\nreason = 'Reviewed'\nvalues = ['fixture-value']\n",
        "[[name_tokens]]\nreason = 'Synthetic'\nvalues = ['sample', 'client']\n",
        "[[non_name_words]]\nreason = 'A typeface'\nvalues = ['serif']\n",
    ))
    .unwrap();
    for (path, skipped) in [
        ("Vendor/pkg/lib.rs", true),
        ("NOTICE", true),
        ("fixtures/a.json", true),
        ("fixtures/ab.json", false),
        ("src/vendor/lib.rs", false),
    ] {
        assert_eq!(allow.skips(path).unwrap(), skipped, "{path}");
    }
    for (entity, value, allowed) in [
        ("CREDIT_CARD", " FIXTURE-VALUE ", true),
        ("CREDIT_CARD", "0000", false),
        ("PERSON", "Sample Client's", true),
        ("PERSON", "Sample Quillstone", false), // pii-scan: allow — synthetic fixture
        ("PERSON", "Quillstone Serif", true),
        ("PHONE_NUMBER", "(702) 555-0100", true),
    ] {
        assert_eq!(allow.allows(entity, value, &patterns), allowed, "{value}");
    }
    for malformed in [
        "[[names]]\nreason = 'No values'\nvalues = []\n",
        "[[names]]\nreason = 'Blank value'\nvalues = [' ']\n",
        "[[names]]\nreason = 'Unknown key'\nvalues = ['Fixture Person']\nowner = 'x'\n",
    ] {
        assert!(Allowlist::parse(malformed).is_err(), "{malformed}");
    }
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
    assert!(gate.contains("docker run --detach --name presidio-analyzer"));
    assert!(gate.contains("--log-driver none"));
    assert!(gate.contains("pii --analyzer-url http://127.0.0.1:3000"));
    assert!(gate.contains("--retry-all-errors"));
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
