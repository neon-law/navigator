//! `navigator` run as a process against an in-process deployment: the parsed
//! arguments become the requests the deployment receives, and its answers
//! become the lines and exit code the lawyer sees.

use serde_json::json;
use uuid::Uuid;

/// A deployment that knows one Project, the directory commands run in, and
/// the stored session that reaches the deployment.
pub struct Site {
    pub server: wiremock::MockServer,
    pub dir: tempfile::TempDir,
    pub project: Uuid,
}

impl Site {
    pub async fn start() -> Self {
        let site = Self::signed_out().await;
        std::fs::write(
            site.dir.path().join("credentials.json"),
            serde_json::to_vec(&json!({
                "hosts": { site.server.uri(): {
                    "token": "navigator-session", "role": "lawyer", "expires_at": 4_102_444_800_i64
                } }
            }))
            .unwrap(),
        )
        .unwrap();
        site
    }

    /// The same deployment with no stored session.
    pub async fn signed_out() -> Self {
        let site = Self {
            server: wiremock::MockServer::start().await,
            dir: tempfile::tempdir().unwrap(),
            project: Uuid::now_v7(),
        };
        let projects = json!([{ "id": site.project, "code": "sample" }]);
        site.answer("GET", "/app/api/projects", 200, projects).await;
        site
    }

    /// Answer `method path` with `status` and a JSON `body`.
    pub async fn answer(&self, method: &str, path: &str, status: u16, body: serde_json::Value) {
        wiremock::Mock::given(wiremock::matchers::method(method))
            .and(wiremock::matchers::path(path))
            .respond_with(wiremock::ResponseTemplate::new(status).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    /// A path below the Project's own API door.
    pub fn door(&self, suffix: &str) -> String {
        format!("/app/api/projects/{}/{suffix}", self.project)
    }

    pub async fn navigator(&self, args: &[&str]) -> Ran {
        self.navigator_reading(args, "").await
    }

    /// Run `navigator` with `input` piped to its standard input.
    pub async fn navigator_reading(&self, args: &[&str], input: &str) -> Ran {
        let mut command = assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("navigator"));
        command
            .current_dir(self.dir.path())
            .args(args)
            .write_stdin(input)
            .env("HOME", self.dir.path())
            .env_remove("NAVIGATOR_CONFIG_DIR")
            .env(
                "NAVIGATOR_CREDENTIALS_FILE",
                self.dir.path().join("credentials.json"),
            );
        let output = tokio::task::spawn_blocking(move || command.output().unwrap())
            .await
            .unwrap();
        Ran {
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// The last request the deployment received: method, path, and JSON body.
    pub async fn last_request(&self) -> (String, String, Option<serde_json::Value>) {
        let requests = self.server.received_requests().await.unwrap();
        let request = requests.last().expect("a request reached the deployment");
        (
            request.method.to_string(),
            request.url.path().to_string(),
            serde_json::from_slice(&request.body).ok(),
        )
    }
}

/// What one `navigator` run exited with and printed.
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Ran {
    #[track_caller]
    pub fn succeeded(&self) -> &str {
        assert_eq!(self.code, Some(0), "{}{}", self.stderr, self.stdout);
        &self.stdout
    }

    #[track_caller]
    pub fn refused(&self, code: i32, reason: &str) {
        assert_eq!(self.code, Some(code), "{}{}", self.stderr, self.stdout);
        assert!(self.stderr.contains(reason), "{}", self.stderr);
    }
}

/// A verb's arguments, then the method, door suffix, and JSON body it sends,
/// and a line it prints.
type DoorCase<'a> = (
    Vec<&'a str>,
    &'a str,
    &'a str,
    Option<serde_json::Value>,
    &'a str,
);

#[tokio::test]
#[allow(clippy::too_many_lines)] // one table of verbs; splitting it scatters the cases
async fn document_verbs_send_one_command_to_the_projects_document_door() {
    let site = Site::start().await;
    let asset = Uuid::now_v7();
    let id = asset.to_string();
    let row = site.door(&format!("documents/{asset}"));
    let filed = json!({
        "slug": "letters/demand.pdf",
        "previous_slug": "demand.pdf",
        "kind": "memo",
        "changed": true,
    });
    for (method, suffix) in [
        ("PATCH", ""),
        ("POST", "/reclassify"),
        ("POST", "/storage"),
        ("POST", "/restore"),
        ("POST", "/hold"),
        ("POST", "/delete"),
    ] {
        site.answer(method, &format!("{row}{suffix}"), 200, filed.clone())
            .await;
    }
    let inventory = json!([{
        "id": asset,
        "byte_size": 9,
        "sha256_hex": "00",
        "filename": "demand.pdf",
        "kind": "memo",
        "visibility": "internal",
        "slug": "letters/demand.pdf",
        "inserted_at": "2099-01-02T00:00:00Z",
    }]);
    site.answer("GET", &site.door("documents"), 200, inventory)
        .await;
    let slug = "letters/demand.pdf";
    let cases: Vec<DoorCase<'_>> = vec![
        (
            vec!["slug", &id, "--slug", slug, "--kind", "memo", "--dry-run"],
            "PATCH",
            "",
            Some(json!({"slug": slug, "dry_run": true, "kind": "memo"})),
            "\"changed\": true",
        ),
        (
            vec!["kind", &id, "--kind", "letter"],
            "PATCH",
            "",
            Some(json!({"kind": "letter", "dry_run": false})),
            "\"kind\": \"memo\"",
        ),
        (
            vec!["reclassify", &id, "--kind", "letter", "--dry-run"],
            "POST",
            "/reclassify",
            Some(json!({"kind": "letter", "dry_run": true})),
            "\"changed\": true",
        ),
        (
            vec!["visibility", &id, "--visibility", "client"],
            "PATCH",
            "",
            Some(json!({"visibility": "client"})),
            "is now client",
        ),
        (
            vec!["reslug", &id, "--slug", slug, "--dry-run"],
            "PATCH",
            "",
            Some(json!({"slug": slug, "reslug": true, "dry_run": true})),
            "\"previous_slug\": \"demand.pdf\"",
        ),
        (
            vec!["repair", &id],
            "POST",
            "/storage",
            Some(json!({"dry_run": false})),
            "\"changed\": true",
        ),
        (
            vec!["restore", &id],
            "POST",
            "/restore",
            None,
            "navigator project sync",
        ),
        (
            vec!["hold", &id],
            "POST",
            "/hold",
            Some(json!({"held": true})),
            "\"changed\": true",
        ),
        (
            vec!["hold", &id, "--release"],
            "POST",
            "/hold",
            Some(json!({"held": false})),
            "\"changed\": true",
        ),
        (
            vec!["retire", &id, "--yes-retire"],
            "POST",
            "/delete",
            Some(json!({"manual": true})),
            "\"slug\": \"letters/demand.pdf\"",
        ),
    ];
    for (verb, method, suffix, body, printed) in cases {
        let mut args = vec!["site", "document"];
        args.extend(&verb);
        args.extend(["--project", "sample"]);
        let ran = site.navigator(&args).await;
        assert!(
            ran.succeeded().contains(printed),
            "{verb:?}: {}",
            ran.stdout
        );
        assert_eq!(
            site.last_request().await,
            (method.to_string(), format!("{row}{suffix}"), body),
            "{verb:?}"
        );
    }

    let retire = ["site", "document", "retire", &id, "--project", "sample"];
    let preview = site
        .navigator(&[&retire[..], &["--dry-run"]].concat())
        .await;
    assert!(
        preview.succeeded().contains("\"revisions\": 1"),
        "{}",
        preview.stdout
    );
    site.navigator(&retire)
        .await
        .refused(2, "pass --yes-retire");
    let sweep = ["site", "document", "slugless", "--project", "sample"];
    site.navigator(&[&sweep[..], &["--dry-run"]].concat())
        .await
        .succeeded();
    site.navigator(&sweep).await.refused(2, "pass --yes");
}

#[tokio::test]
async fn mail_file_previews_the_attachments_a_message_would_file() {
    let site = Site::start().await;
    let message = Uuid::now_v7().to_string();
    let mut args = vec!["site", "mail", "file", "--project", "sample", "--kind"];
    args.extend(["exhibit", "--message", &message, "--dry-run"]);
    let attachments = json!({
        "attachments": [
            { "filename": "lease.pdf", "content_type": "application/pdf", "size_bytes": 12 }
        ],
        "filed": [],
    });
    site.answer("POST", &site.door("mail/file"), 200, attachments)
        .await;

    let ran = site.navigator(&args).await;

    assert!(
        ran.succeeded()
            .contains("would file lease.pdf (application/pdf, 12 bytes)"),
        "{}",
        ran.stdout
    );
    let (_, _, body) = site.last_request().await;
    assert_eq!(
        body,
        Some(json!({
            "message_id": message, "kind": "exhibit", "visibility": "internal", "dry_run": true
        }))
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one table of verbs; splitting it scatters the cases
async fn notation_verbs_walk_one_notation_through_review() {
    let site = Site::start().await;
    let notation = Uuid::now_v7();
    let id = notation.to_string();
    let lawyer = format!("/app/lawyer/notations/{notation}");
    let api = format!("/app/api/notations/{notation}");
    let review = json!({ "state": "lawyer_review", "delivery": "emailed", "document_ready": true });
    site.answer("GET", &format!("{lawyer}/review"), 200, review)
        .await;
    for step in ["approve-send", "request-changes", "reask"] {
        site.answer("POST", &format!("{lawyer}/{step}"), 200, json!({}))
            .await;
    }
    site.answer(
        "POST",
        &site.door("notations"),
        201,
        json!({ "notation_id": notation }),
    )
    .await;
    let sent = json!({ "state": "awaiting_signature", "signature_request_id": "envelope-1" });
    site.answer("POST", &format!("{api}/signature"), 200, sent.clone())
        .await;
    site.answer("GET", &format!("{api}/signature"), 200, sent)
        .await;
    let answer = json!([{
        "id": Uuid::now_v7(),
        "question_id": Uuid::now_v7(),
        "state_name": "client_name",
        "value": "Sample Client",
        "source": "client",
        "authored_by_person_id": null,
    }]);
    site.answer("GET", &format!("{api}/answers"), 200, answer)
        .await;
    let board = json!([{
        "template_code": "onboarding__letter",
        "notation_id": id,
        "current_state": "lawyer_review",
        "workflow_states": ["BEGIN", "lawyer_review", "END"],
        "state_entered_at": {},
        "respondent_email": "client@example.com",
        "signature_request_id": null,
        "last_transition_at": null,
    }]);
    site.answer("GET", &site.door("notation-inventory"), 200, board)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(format!(
            "{lawyer}/documents/document"
        )))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(b"%PDF-1.7".to_vec()))
        .mount(&site.server)
        .await;

    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec![
                "create",
                "retainer__letter",
                "--project",
                "sample",
                "--client-email",
                "client@example.com",
            ],
            "created notation",
        ),
        (vec!["list", "--project", "sample"], "onboarding__letter"),
        (
            vec!["list", "--project", "sample", "--json"],
            "\"respondent_email\": \"client@example.com\"",
        ),
        (vec!["status", &id], "lawyer_review"),
        (vec!["status", &id, "--json"], "\"delivery\": \"emailed\""),
        (vec!["answers", &id, "--json"], "\"source\": \"client\""),
        (vec!["approve", &id], "approval submitted"),
        (
            vec![
                "request-changes",
                &id,
                "--question",
                "client_name",
                "--note",
                "check the spelling",
            ],
            "flagged client_name",
        ),
        (
            vec!["update", &id, "--answer", "client_name=Sample Client"],
            "re-collected and resubmitted",
        ),
        (vec!["document", &id, "--out", "packet.pdf"], "(8 bytes)"),
    ];
    for (verb, printed) in cases {
        let ran = site
            .navigator(&[&["site", "notation"][..], &verb].concat())
            .await;
        assert!(
            ran.succeeded().contains(printed),
            "{verb:?}: {}",
            ran.stdout
        );
    }
    assert_eq!(
        std::fs::read(site.dir.path().join("packet.pdf")).unwrap(),
        b"%PDF-1.7"
    );

    for (verb, reason) in [
        (vec!["request-changes", &id], "at least one --question"),
        (vec!["update", &id], "at least one --answer"),
        (
            vec!["update", &id, "--answer", "client_name"],
            "must be `code=value`",
        ),
        (
            vec![
                "create",
                "offboarding",
                "--project",
                "sample",
                "--client-email",
                "client@example.com",
                "--answers-from",
                "answers.yaml",
            ],
            "offboarding takes no answers file",
        ),
    ] {
        let ran = site
            .navigator(&[&["site", "notation"][..], &verb].concat())
            .await;
        assert_ne!(ran.code, Some(0), "{verb:?}");
        assert!(ran.stderr.contains(reason), "{verb:?}: {}", ran.stderr);
    }
}

#[tokio::test]
async fn every_live_site_verb_refuses_without_a_stored_session() {
    let site = Site::signed_out().await;
    let id = Uuid::now_v7().to_string();
    let project = ["--project", "sample"];
    let cases: Vec<Vec<&str>> = vec![
        vec!["site", "document", "slug", &id, "--slug", "a.pdf"],
        vec!["site", "document", "kind", &id, "--kind", "memo"],
        vec!["site", "document", "reclassify", &id, "--kind", "memo"],
        vec![
            "site",
            "document",
            "visibility",
            &id,
            "--visibility",
            "client",
        ],
        vec!["site", "document", "reslug", &id, "--slug", "a.pdf"],
        vec!["site", "document", "retire", &id],
        vec!["site", "document", "slugless", "--dry-run"],
        vec!["site", "document", "restore", &id],
        vec!["site", "document", "hold", &id],
        vec!["site", "document", "repair", &id],
        vec!["site", "mail", "file", "--message", &id, "--kind", "memo"],
        vec!["site", "notation", "list"],
    ];
    for case in cases {
        let ran = site.navigator(&[&case[..], &project].concat()).await;
        ran.refused(2, "no stored logins");
    }
    for case in [
        vec!["site", "notation", "status", &id],
        vec!["site", "notation", "answers", &id],
        vec!["site", "notation", "approve", &id],
        vec!["notation", "onboarding"],
    ] {
        site.navigator(&case).await.refused(2, "no stored logins");
    }
    assert!(site.server.received_requests().await.unwrap().is_empty());
}
