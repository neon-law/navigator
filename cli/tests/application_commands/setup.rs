//! `navigator legal-plans`, `navigator project legal-plan`, `navigator site
//! login|logout|whoami`, and the local `navigator notation` renders, each run
//! as a process: its arguments become requests and files, and what it prints
//! and exits with is what the lawyer sees.

use std::io::{BufRead, Read, Write};
use std::path::Path;

use serde_json::{json, Value};

use crate::site::{Ran, Site};

const FIRM: &str = "0192f0a0-0000-7000-8000-000000000003";
const CLAIM: &str = "0192f0a0-0000-7000-8000-000000000004";

/// Run `navigator` in `dir` with its home, session file, and render caches
/// all below `dir`, and `env` applied last.
async fn navigator_in(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Ran {
    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("navigator"));
    command
        .current_dir(dir)
        .args(args)
        .env("HOME", dir)
        .env("TMPDIR", dir)
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("NAVIGATOR_CREDENTIALS_FILE", dir.join("credentials.json"))
        .env_remove("NAVIGATOR_CONFIG_DIR")
        .env_remove("NAVIGATOR_LETTERHEAD_API_BASE")
        .env_remove("NAVIGATOR_LETTERHEAD_CACHE")
        .env_remove("NAVIGATOR_FORM_CACHE")
        .envs(env.iter().copied());
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    Ran {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// How many requests the deployment received for `path`.
async fn asked(server: &wiremock::MockServer, path: &str) -> usize {
    let requests = server.received_requests().await.unwrap();
    requests.iter().filter(|r| r.url.path() == path).count()
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one table of verbs; splitting it scatters the cases
async fn legal_plan_verbs_call_the_firm_and_project_doors() {
    let site = Site::start().await;
    let plans = format!("/app/api/firms/{FIRM}/legal-plans");
    let collections = format!("/app/api/firms/{FIRM}/legal-plan-collections");
    let plan = site.door("legal-plan");
    let claims = site.door("legal-plan-claims");
    for (method, path) in [
        ("GET", &plans),
        ("POST", &plans),
        ("GET", &collections),
        ("GET", &plan),
        ("PATCH", &plan),
        ("GET", &claims),
    ] {
        site.answer(method, path, 200, json!({ "name": "Sample Panel Plan" }))
            .await;
    }
    let created = json!({
        "name": "Sample Panel Plan",
        "portal": "https://plan.example.com/account",
        "contact": null,
        "acceptance_sla_hours": 24,
        "turnaround_sla_days": null,
        "reporting_requirements": "Monthly",
    });
    let cases: Vec<(Vec<&str>, &str, &str, Option<Value>)> = vec![
        (
            vec!["legal-plans", "--firm", FIRM, "list"],
            "GET",
            &plans,
            None,
        ),
        (
            vec![
                "legal-plans",
                "--firm",
                FIRM,
                "collections",
                "--totals-only",
            ],
            "GET",
            &collections,
            None,
        ),
        (
            vec![
                "legal-plans",
                "--firm",
                FIRM,
                "create",
                "--name",
                "Sample Panel Plan",
                "--portal",
                "https://plan.example.com/account",
                "--acceptance-sla-hours",
                "24",
                "--reporting-requirements",
                "Monthly",
            ],
            "POST",
            &plans,
            Some(created),
        ),
        (
            vec!["project", "legal-plan", "show", "--project", "sample"],
            "GET",
            &plan,
            None,
        ),
        (
            vec![
                "project",
                "legal-plan",
                "set",
                "--project",
                "sample",
                "--plan",
                "ARAG",
                "--case-number",
                "",
            ],
            "PATCH",
            &plan,
            Some(json!({ "legal_plan": "ARAG", "plan_case_number": "" })),
        ),
        (
            vec![
                "project",
                "legal-plan",
                "claim",
                "--project",
                "sample",
                "status",
            ],
            "GET",
            &claims,
            None,
        ),
    ];
    for (args, method, path, body) in cases {
        let ran = site.navigator(&args).await;
        assert_eq!(
            ran.succeeded().trim(),
            "{\n  \"name\": \"Sample Panel Plan\"\n}",
            "{args:?}"
        );
        assert_eq!(
            site.last_request().await,
            (method.to_string(), path.to_string(), body),
            "{args:?}"
        );
    }
    let requests = site.server.received_requests().await.unwrap();
    let totals = requests.iter().find(|r| r.url.path() == collections);
    assert_eq!(totals.unwrap().url.query(), Some("totals_only=true"));

    for (args, reason) in [
        (
            vec!["project", "legal-plan", "set", "--project", "sample"],
            "name at least one field to set",
        ),
        (
            vec!["legal-plans", "--firm", "Unknown Practice", "list"],
            "pass a Firm UUID if you lack directory access",
        ),
        (
            vec!["legal-plans", "--firm", CLAIM, "list"],
            "legal plan list refused: 404",
        ),
    ] {
        site.navigator(&args).await.refused(1, reason);
    }
    let signed_out = Site::signed_out().await;
    for args in [
        vec!["legal-plans", "--firm", FIRM, "list"],
        vec!["project", "legal-plan", "show", "--project", "sample"],
    ] {
        signed_out
            .navigator(&args)
            .await
            .refused(1, "no stored logins");
    }
}

#[tokio::test]
async fn filing_review_prints_each_carriers_unfiled_claims() {
    let site = Site::start().await;
    let collections = format!("/app/api/firms/{FIRM}/legal-plan-collections");
    let review = json!({ "filing_review": {
        "ARAG": { "eligible_unsubmitted_count": 2, "eligible_unsubmitted_cents": 45_000 },
        "LegalEASE": { "eligible_unsubmitted_count": 0, "eligible_unsubmitted_cents": 0 },
    } });
    site.answer("GET", &collections, 200, review).await;
    let args = [
        "legal-plans",
        "--firm",
        FIRM,
        "collections",
        "--filing-review",
    ];

    let ran = site.navigator(&args).await;

    let rows: Vec<Vec<&str>> = ran
        .succeeded()
        .lines()
        .skip(1)
        .map(|line| line.split_whitespace().collect())
        .collect();
    assert_eq!(
        rows,
        [["ARAG", "2", "$450.00"], ["LegalEASE", "0", "$0.00"]],
        "{}",
        ran.stdout
    );

    let without_totals = Site::start().await;
    without_totals
        .answer("GET", &collections, 200, json!({ "plans": {} }))
        .await;
    without_totals
        .navigator(&args)
        .await
        .refused(1, "the site did not return complete filing-review totals");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one table of verbs; splitting it scatters the cases
async fn claim_verbs_post_one_lawyer_confirmed_event_each() {
    let site = Site::start().await;
    let claims = site.door("legal-plan-claims");
    site.answer("POST", &claims, 200, json!({ "recorded": true }))
        .await;
    let amount = |kind: &str, reference: &str| json!({ "type": kind, "reference": reference, "cents": 4000 });
    let cases: Vec<(Vec<&str>, Value)> = vec![
        (
            vec![
                "authorize",
                "--authorization-reference",
                "sample-approval",
                "--fixed-cents",
                "5000",
            ],
            json!({ "type": "authorization_confirmed", "authorization": {
                "type": "fixed_fee", "reference": "sample-approval", "cents": 5000,
            } }),
        ),
        (vec!["prepare"], json!({ "type": "prepared" })),
        (
            vec!["file", "--provider-reference", "sample-confirmation"],
            json!({
                "type": "submission", "provider_reference": "sample-confirmation",
                "claimed_cents": null,
            }),
        ),
        (
            vec![
                "correct",
                "--provider-reference",
                "sample-correction",
                "--claimed-cents",
                "4000",
            ],
            json!({
                "type": "correction", "provider_reference": "sample-correction",
                "claimed_cents": 4000,
            }),
        ),
        (
            vec![
                "approve",
                "--reference",
                "sample-decision",
                "--cents",
                "4000",
            ],
            amount("approval", "sample-decision"),
        ),
        (
            vec!["deny", "--reference", "sample-decision"],
            json!({ "type": "denial", "reference": "sample-decision" }),
        ),
        (
            vec![
                "record-payment",
                "--reference",
                "sample-remittance",
                "--cents",
                "4000",
            ],
            amount("payment", "sample-remittance"),
        ),
        (
            vec!["reverse", "--reference", "sample-refund", "--cents", "4000"],
            amount("reversal", "sample-refund"),
        ),
    ];
    let claim = |verb: &[&'static str]| {
        let mut args = vec!["project", "legal-plan", "claim", "--project", "sample"];
        args.extend([verb[0], "--claim", CLAIM, "--on", "2099-01-02"]);
        args.extend(&verb[1..]);
        args
    };
    let mut event_ids = std::collections::BTreeSet::new();
    for (verb, event) in &cases {
        let ran = site.navigator(&claim(verb)).await;
        assert!(ran.succeeded().contains("\"recorded\": true"), "{verb:?}");
        let (method, path, body) = site.last_request().await;
        let body = body.unwrap();
        assert_eq!((method.as_str(), &path), ("POST", &claims), "{verb:?}");
        assert_eq!(body["claim_id"], CLAIM, "{verb:?}");
        assert_eq!(body["occurred_on"], "2099-01-02", "{verb:?}");
        assert_eq!(&body["event"], event, "{verb:?}");
        event_ids.insert(body["event_id"].as_str().unwrap().to_string());
    }
    assert_eq!(event_ids.len(), cases.len(), "each event has its own key");
    site.navigator(&claim(&["prepare"])).await.succeeded();
    let (_, _, repeated) = site.last_request().await;
    let repeated = repeated.unwrap()["event_id"].as_str().unwrap().to_string();
    assert!(
        event_ids.contains(&repeated),
        "a repeated event keeps its key"
    );

    for (verb, reason) in [
        (vec!["authorize"], "supply verified authorization terms"),
        (
            vec!["authorize", "--authorization-reference", "sample-approval"],
            "either a fixed fee or capped hourly terms",
        ),
    ] {
        site.navigator(&claim(&verb)).await.refused(1, reason);
    }
}

#[tokio::test]
async fn claim_work_and_recorded_events_reach_the_claims_door() {
    let site = Site::start().await;
    let claims = site.door("legal-plan-claims");
    site.answer("POST", &claims, 200, json!({ "recorded": true }))
        .await;
    let claim = ["project", "legal-plan", "claim", "--project", "sample"];
    let (first, second) = (
        "0192f0a0-0000-7000-8000-00000000000a",
        "0192f0a0-0000-7000-8000-00000000000b",
    );
    let mut work = claim.to_vec();
    work.extend([
        "work",
        "--service",
        "arag-representation",
        "--benefit",
        "SAMPLE",
    ]);
    work.extend(["--performed-on", "2099-01-02", "--time-entry", second]);
    work.extend(["--time-entry", first]);

    site.navigator(&work).await.succeeded();

    let (_, _, body) = site.last_request().await;
    let body = body.unwrap();
    assert_eq!(body["occurred_on"], "2099-01-02");
    assert_eq!(
        body["event"],
        json!({ "type": "work_recorded", "work": {
            "service": "arag_representation", "benefit": "SAMPLE", "performed_on": "2099-01-02",
            "attorney_minutes": null, "time_entry_ids": [first, second], "authorization": null,
        } })
    );

    let event = json!({
        "claim_id": CLAIM, "event_id": FIRM, "occurred_on": "2099-01-02",
        "event": { "type": "prepared" },
    });
    std::fs::write(site.dir.path().join("event.json"), event.to_string()).unwrap();
    let from_file = [&claim[..], &["record", "--input", "event.json"]].concat();
    site.navigator(&from_file).await.succeeded();
    assert_eq!(site.last_request().await.2, Some(event.clone()));
    let from_stdin = [&claim[..], &["record", "--input", "-"]].concat();
    site.navigator_reading(&from_stdin, &event.to_string())
        .await
        .succeeded();
    assert_eq!(site.last_request().await.2, Some(event));

    site.navigator_reading(&from_stdin, "not an event")
        .await
        .refused(1, "claim event must be valid JSON");
    let missing = [&claim[..], &["record", "--input", "missing.json"]].concat();
    site.navigator(&missing)
        .await
        .refused(1, "open private claim event");
}

/// Replace the stored sessions with one for `host`.
fn store_session(site: &Site, host: &str, session: &Value) {
    std::fs::write(
        site.dir.path().join("credentials.json"),
        json!({ "hosts": { host: session } }).to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn whoami_and_logout_read_and_forget_the_stored_session() {
    let site = Site::start().await;
    let host = site.server.uri();

    let who = site.navigator(&["site", "whoami"]).await;
    assert!(
        who.succeeded()
            .starts_with("(unknown email) (lawyer) — expires in "),
        "{}",
        who.stdout
    );
    let elsewhere = ["--host", "deployment.example.com"];
    site.navigator(&[&["site", "whoami"][..], &elsewhere].concat())
        .await
        .refused(1, "not logged in to https://deployment.example.com");
    let other = site
        .navigator(&[&["site", "logout"][..], &elsewhere].concat())
        .await;
    assert_eq!(
        other.succeeded().trim(),
        "no stored login for https://deployment.example.com"
    );

    let expired = json!({
        "token": "navigator-session", "person_email": "lawyer@example.com",
        "role": "lawyer", "expires_at": 1,
    });
    store_session(&site, &host, &expired);
    let stale = site.navigator(&["site", "whoami"]).await;
    assert_eq!(stale.code, Some(1), "{}", stale.stderr);
    assert!(
        stale
            .stdout
            .starts_with("lawyer@example.com (lawyer) — token expired (run `navigator site login"),
        "{}",
        stale.stdout
    );

    let out = site.navigator(&["site", "logout"]).await;
    assert_eq!(out.succeeded().trim(), format!("logged out of {host}"));
    for verb in ["whoami", "logout"] {
        site.navigator(&["site", verb])
            .await
            .refused(2, "no stored logins");
    }

    std::fs::write(site.dir.path().join("credentials.json"), "not a session").unwrap();
    for verb in ["whoami", "logout"] {
        let reason = format!("navigator site {verb}: parse ");
        site.navigator(&["site", verb]).await.refused(2, &reason);
    }
}

/// Run `navigator site login --no-browser` against `site` and answer its
/// loopback listener with `callback`, where `{state}` stands for the state
/// the login minted.
async fn login(site: &Site, callback: &'static str) -> Ran {
    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin!("navigator"));
    command
        .current_dir(site.dir.path())
        .args([
            "site",
            "login",
            "--no-browser",
            "--host",
            &site.server.uri(),
        ])
        .env("HOME", site.dir.path())
        .env_remove("NAVIGATOR_CONFIG_DIR")
        .env(
            "NAVIGATOR_CREDENTIALS_FILE",
            site.dir.path().join("credentials.json"),
        )
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    tokio::task::spawn_blocking(move || {
        let mut child = command.spawn().unwrap();
        let mut printed = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut start = String::new();
        while !start.contains("/auth/cli/start?") {
            start.clear();
            assert_ne!(printed.read_line(&mut start).unwrap(), 0, "no login URL");
        }
        let start = url::Url::parse(start.trim()).unwrap();
        let query: std::collections::BTreeMap<_, _> = start.query_pairs().into_owned().collect();
        let redirect = url::Url::parse(&query["redirect"]).unwrap();
        let loopback = ("127.0.0.1", redirect.port().unwrap());
        let mut socket = std::net::TcpStream::connect(loopback).unwrap();
        let callback = callback.replace("{state}", &query["state"]);
        write!(socket, "GET /cb?{callback} HTTP/1.1\r\n\r\n").unwrap();
        let _ = socket.read_to_end(&mut Vec::new());
        let (mut stdout, mut stderr) = (String::new(), String::new());
        printed.read_to_string(&mut stdout).unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        Ran {
            code: child.wait().unwrap().code(),
            stdout,
            stderr,
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn login_stores_only_a_session_the_deployment_accepts() {
    let site = Site::signed_out().await;
    let stored = site.dir.path().join("credentials.json");
    login(&site, "token=minted-session&state=tampered")
        .await
        .refused(2, "state mismatch");
    login(&site, "error=access_denied&state={state}")
        .await
        .refused(2, "login failed: access_denied");
    login(&site, "token=minted-session&state={state}")
        .await
        .refused(2, "whoami check failed: 404");
    assert!(!stored.exists(), "a refused login stores nothing");

    let identity = json!({
        "email": "lawyer@example.com", "role": "lawyer", "exp": 4_102_444_800_i64,
    });
    site.answer("GET", "/auth/cli/whoami", 200, identity).await;
    let ran = login(&site, "token=minted-session&state={state}").await;
    let host = site.server.uri();
    assert!(
        ran.succeeded().contains(&format!(
            "logged in to {host} as lawyer@example.com (lawyer)"
        )),
        "{}",
        ran.stdout
    );
    let saved: Value = serde_json::from_slice(&std::fs::read(stored).unwrap()).unwrap();
    assert_eq!(
        saved["hosts"][&host],
        json!({
            "token": "minted-session", "person_email": "lawyer@example.com",
            "role": "lawyer", "expires_at": 4_102_444_800_i64,
        })
    );
    let who = site.navigator(&["site", "whoami"]).await;
    assert!(who.succeeded().starts_with("lawyer@example.com (lawyer)"));
}

const FRONTMATTER_TAIL: &str = "respondent_type: person\nconfidential: true\nquestionnaire:\n  BEGIN:\n    _: END\n  END: {}\nworkflow:\n  BEGIN:\n    intake_submitted: lawyer_review\n  lawyer_review:\n    approved: END\n    rejected: END\n  END: {}\n---\n";

/// A will with one fact to fill, staged as `draft.md` in a new directory.
fn draft() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let will = format!(
        "---\nkind: will\ntitle: Draft Will\n{FRONTMATTER_TAIL}\n# Draft Will\n\n## I. Disposition\n\nI give my property to {{{{beneficiary}}}}.\n"
    );
    std::fs::write(dir.path().join("draft.md"), will).unwrap();
    std::fs::write(
        dir.path().join("answers.yaml"),
        "beneficiary: Sample Client\n",
    )
    .unwrap();
    dir
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one table of verbs; splitting it scatters the cases
async fn local_render_verbs_write_the_document_each_one_names() {
    let dir = draft();
    let at = |name: &str| dir.path().join(name);
    let answered = ["--answers-file", "answers.yaml"];

    let kinds = navigator_in(dir.path(), &["notation", "kinds"], &[]).await;
    assert!(kinds.succeeded().contains("pleading"), "{}", kinds.stdout);

    let mut preview = vec!["notation", "preview", "draft.md", "--no-open"];
    preview.extend(answered);
    preview.extend(["--answer", "beneficiary=recipient"]);
    let previewed = navigator_in(dir.path(), &preview, &[]).await;
    previewed.succeeded();
    assert!(
        previewed
            .stderr
            .contains("an explicit --answer overrides a YAML field"),
        "{}",
        previewed.stderr
    );
    let previews = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("navigator-preview-")
                && std::path::Path::new(&name)
                    .extension()
                    .is_some_and(|ext| ext == "pdf")
        })
        .count();
    assert_eq!(
        previews, 1,
        "the unnamed preview lands in the temp directory"
    );

    let mut pdf = vec!["notation", "pdf", "draft.md", "--out", "final.pdf"];
    pdf.extend(answered);
    navigator_in(dir.path(), &pdf, &[]).await.succeeded();
    assert!(std::fs::read(at("final.pdf"))
        .unwrap()
        .starts_with(b"%PDF-"));

    let word = ["notation", "word", "draft.md", "--preview", "--out"];
    navigator_in(dir.path(), &[&word[..], &["first.docx"]].concat(), &[])
        .await
        .succeeded();
    assert!(std::fs::read(at("first.docx")).unwrap().starts_with(b"PK"));
    let mut redline = [&word[..], &["second.docx", "--redline-against"]].concat();
    redline.extend(["first.docx", "--redline-author", "Sample Counsel"]);
    let redlined = navigator_in(dir.path(), &redline, &[]).await;
    assert!(
        redlined
            .succeeded()
            .contains("No differences from the previous version"),
        "{}",
        redlined.stdout
    );

    let cases: Vec<(Vec<&str>, i32, &str)> = vec![
        (
            vec![
                "preview",
                "draft.md",
                "--no-open",
                "--answers-file",
                "none.yaml",
            ],
            1,
            "read answers file none.yaml",
        ),
        (
            vec![
                "pdf",
                "draft.md",
                "--out",
                "open.pdf",
                "--answers-file",
                "none.yaml",
            ],
            2,
            "read answers file none.yaml",
        ),
        (
            vec![
                "pdf",
                "draft.md",
                "--out",
                "open.pdf",
                "--answers-from",
                "latest",
            ],
            2,
            "--answers-from requires a Project checkout",
        ),
        (
            vec!["pdf", "draft.md", "--out", "open.pdf"],
            1,
            "a final render carries no unfinished text",
        ),
        (
            vec!["pdf", "draft.md", "--out", "open.docx", "--preview"],
            2,
            "output file extension does not match the render format",
        ),
        (
            vec!["word", "none.md", "--out", "open.docx"],
            2,
            "could not read the template file",
        ),
        (
            vec!["import", "draft.md", "none.docx", "--dry-run"],
            2,
            "could not read the edited Word file",
        ),
    ];
    for (verb, code, reason) in cases {
        let args = [&["notation"][..], &verb].concat();
        navigator_in(dir.path(), &args, &[])
            .await
            .refused(code, reason);
    }
    assert!(!at("open.pdf").exists() && !at("open.docx").exists());
}

/// A letter in a Project whose `navigator.yaml` selects the Vesta Brand by
/// host, staged as `demand.md`.
fn letter_in_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let host = views::brand::BrandKey::Vesta.canonical_host();
    let manifest = format!("version: v26.10.6\nproject:\n  name: sample\n  host: {host}\n");
    std::fs::write(dir.path().join("navigator.yaml"), manifest).unwrap();
    let letter = format!(
        "---\nkind: letter\ntitle: Test Demand\noutput: letter\n{FRONTMATTER_TAIL}\n# Demand\n\nPay the sum of `{{{{amount}}}}` without delay.\n"
    );
    std::fs::write(dir.path().join("demand.md"), letter).unwrap();
    dir
}

/// A deployment whose letterhead door answers `row` for the Vesta Brand.
async fn letterhead_door(row: &Value) -> wiremock::MockServer {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/app/api/brands/vesta/letterhead"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(row))
        .mount(&server)
        .await;
    server
}

/// The header part of a rendered Word letter.
fn word_header(docx: &Path) -> String {
    let file = std::fs::File::open(docx).unwrap();
    let mut archive = zip::ZipArchive::new(file).unwrap();
    let mut xml = String::new();
    archive
        .by_name("word/header2.xml")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml
}

const WORD_LETTER: [&str; 6] = [
    "notation",
    "word",
    "demand.md",
    "--preview",
    "--out",
    "demand.docx",
];
const PDF_LETTER: [&str; 7] = [
    "notation",
    "pdf",
    "demand.md",
    "--preview",
    "--ci",
    "--out",
    "demand.pdf",
];
const DOOR: &str = "NAVIGATOR_LETTERHEAD_API_BASE";

#[tokio::test]
async fn a_letter_wears_the_letterhead_row_its_deployment_serves() {
    let dir = letter_in_project();
    let row = json!({
        "brand_key": "vesta", "wordmark": "Sample Estate Planning", "mark": "wordmark",
        "firm_legal_name": "Sample Estate Planning PC", "phone": "+1 555 010 0001",
        "email": "letters@example.com", "web": "www.example.com",
        "postal_address": "1 Main Street\nSpringfield, California 90000",
        "font_family": "Sample Serif", "font_object_key": "brands/vesta/fonts/Sample-Serif.woff2",
        "updated_at": "2099-01-02T00:00:00Z",
    });
    let server = letterhead_door(&row).await;
    let door = [(DOOR, server.uri())];
    let door = [(door[0].0, door[0].1.as_str())];

    let ran = navigator_in(dir.path(), &WORD_LETTER, &door).await;

    ran.succeeded();
    assert!(
        ran.stderr.contains(
            "letterhead: source=runtime brand=vesta revision=2099-01-02T00:00:00Z \
             mark=wordmark font=Sample-Serif.woff2"
        ),
        "{}",
        ran.stderr
    );
    assert!(!ran.stderr.contains("source=compiled"), "{}", ran.stderr);
    let header = word_header(&dir.path().join("demand.docx"));
    assert!(header.contains("SAMPLE ESTATE PLANNING"), "{header}");

    // The warmed cache is the pin: a `--ci` render reads it, never the door.
    let pinned = navigator_in(dir.path(), &PDF_LETTER, &door).await;
    pinned.succeeded();
    assert!(
        pinned.stderr.contains("source=runtime brand=vesta"),
        "{}",
        pinned.stderr
    );
    assert_eq!(asked(&server, "/app/api/brands/vesta/letterhead").await, 1);
}

#[tokio::test]
async fn a_letter_draws_the_uploaded_mark_or_says_why_it_could_not() {
    let row = json!({
        "brand_key": "vesta", "wordmark": "Sample Estate Planning", "mark": "logo",
        "logo_object_key": "brands/vesta/mark.png",
    });
    let unfetchable = letterhead_door(&row).await;
    let fetchable = letterhead_door(&row).await;
    wiremock::Mock::given(wiremock::matchers::path("/assets/brands/vesta/mark.png"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_bytes(pdf::firm_logo_png().to_vec()),
        )
        .mount(&fetchable)
        .await;
    for (server, mark, degraded) in [
        (&fetchable, "logo", false),
        (&unfetchable, "wordmark", true),
    ] {
        let dir = letter_in_project();
        let uri = server.uri();
        let ran = navigator_in(dir.path(), &WORD_LETTER, &[(DOOR, &uri)]).await;
        ran.succeeded();
        let provenance = format!("letterhead: source=runtime brand=vesta revision=n/a mark={mark}");
        assert!(ran.stderr.contains(&provenance), "{mark}: {}", ran.stderr);
        assert_eq!(
            ran.stderr
                .contains("the Brand's uploaded mark could not be drawn"),
            degraded,
            "{mark}: {}",
            ran.stderr
        );
    }
}

#[tokio::test]
async fn a_letter_falls_back_to_the_compiled_letterhead_and_ci_refuses_to() {
    let compiled = "letterhead: source=compiled brand=vesta revision=n/a mark=wordmark";
    let unreachable = "deployment unreachable";
    let dead = [(DOOR, "http://127.0.0.1:1")];

    // A deployment that knows no such Brand answers 404: the compiled frame,
    // with nothing to announce.
    let dir = letter_in_project();
    let unknown = wiremock::MockServer::start().await;
    let uri = unknown.uri();
    let ran = navigator_in(dir.path(), &WORD_LETTER, &[(DOOR, &uri)]).await;
    ran.succeeded();
    assert!(ran.stderr.contains(compiled), "{}", ran.stderr);
    assert!(!ran.stderr.contains(unreachable), "{}", ran.stderr);

    let ran = navigator_in(dir.path(), &WORD_LETTER, &dead).await;
    ran.succeeded();
    assert!(ran.stderr.contains(compiled), "{}", ran.stderr);
    assert!(ran.stderr.contains(unreachable), "{}", ran.stderr);
    let wordmark = views::brand::BrandKey::Vesta
        .resolve_branding(&views::brand::DEFAULT_BRANDING)
        .firm
        .site_name
        .to_uppercase();
    let header = word_header(&dir.path().join("demand.docx"));
    assert!(header.contains(&wordmark), "{header}");

    navigator_in(dir.path(), &PDF_LETTER, &dead).await.refused(
        2,
        "--ci refuses a letter whose letterhead row could not be read",
    );
    assert!(!dir.path().join("demand.pdf").exists());

    // With no door override the Brand's own stored session is the only way
    // in: none, or an expired one, falls back and names the login to run.
    let ran = navigator_in(dir.path(), &WORD_LETTER, &[]).await;
    ran.succeeded();
    assert!(
        ran.stderr.contains("no `navigator site login` session for"),
        "{}",
        ran.stderr
    );
    let host = views::brand::BrandKey::Vesta.canonical_host();
    let expired = json!({ "hosts": { format!("https://{host}"): {
        "token": "navigator-session", "role": "lawyer", "expires_at": 1,
    } } });
    std::fs::write(dir.path().join("credentials.json"), expired.to_string()).unwrap();
    let ran = navigator_in(dir.path(), &WORD_LETTER, &[]).await;
    ran.succeeded();
    assert!(
        ran.stderr
            .contains("has expired — run `navigator site login"),
        "{}",
        ran.stderr
    );
}

const FORM_CODE: &str = "us__naturalization";
const FORM_SHELF: &str = "templates/notations/forms/united_states/federal/uscis";

/// A deployment and checkout for one form render: the vendored
/// naturalization template beside the pin of `pinned`, with no working copy
/// of the blank, so the render asks the deployment, which serves `served`.
async fn form_checkout(pinned: &[u8], served: &[u8]) -> Site {
    let site = Site::start().await;
    let shelf = site.dir.path().join(FORM_SHELF);
    std::fs::create_dir_all(&shelf).unwrap();
    let vendored = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(FORM_SHELF)
        .join(format!("{FORM_CODE}.md"));
    std::fs::copy(vendored, shelf.join("us__n400_sample.md")).unwrap();
    std::fs::write(
        shelf.join(format!("{FORM_CODE}.sha256")),
        forms::sha256_hex(pinned),
    )
    .unwrap();
    wiremock::Mock::given(wiremock::matchers::path(format!(
        "/app/api/forms/{FORM_CODE}/blank"
    )))
    .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(served.to_vec()))
    .mount(&site.server)
    .await;
    site
}

#[tokio::test]
async fn a_form_blank_is_fetched_once_against_its_pin_then_read_from_the_cache() {
    let (given, family) = ("person__client.given", "person__client.family");
    let blank = pdf::blank_acroform(&[given, family]);
    let template = format!("{FORM_SHELF}/us__n400_sample.md");
    let mut render = vec!["notation", "pdf", template.as_str(), "--out", "filled.pdf"];
    render.extend(["--answer", "person__client.given=Sample"]);
    render.extend(["--answer", "person__client.family=Client"]);
    let door = format!("/app/api/forms/{FORM_CODE}/blank");

    let site = form_checkout(&blank, &blank).await;
    let ran = navigator_in(site.dir.path(), &render, &[]).await;
    assert!(
        ran.succeeded()
            .contains("2 field(s) filled, unflattened for review"),
        "{}",
        ran.stdout
    );
    let filled = std::fs::read(site.dir.path().join("filled.pdf")).unwrap();
    assert_eq!(
        pdf::read_field_value(&filled, given).as_deref(),
        Some("Sample")
    );
    assert_eq!(
        pdf::read_field_value(&filled, family).as_deref(),
        Some("Client")
    );

    let pinned = [&render[..], &["--ci"]].concat();
    navigator_in(site.dir.path(), &pinned, &[])
        .await
        .succeeded();
    assert_eq!(asked(&site.server, &door).await, 1);

    let contradicted = form_checkout(&blank, b"%PDF-1.7 another blank").await;
    navigator_in(contradicted.dir.path(), &render, &[])
        .await
        .refused(2, "blank does not match its `.sha256` pin");
    navigator_in(contradicted.dir.path(), &pinned, &[])
        .await
        .refused(2, "--ci refuses to fetch the `us__naturalization` blank");
    assert_eq!(asked(&contradicted.server, &door).await, 1);
    assert!(!contradicted.dir.path().join("filled.pdf").exists());
}

#[tokio::test]
async fn workspace_sync_uses_the_verified_mirror_for_a_new_and_existing_checkout() {
    use std::process::Command;

    const ORIGIN: &str = "https://github.com/example-firm/sample";
    const MIRROR: &str = "https://forge.example.test/example-firm/sample";

    if !Command::new("jj")
        .arg("--version")
        .output()
        .is_ok_and(|r| r.status.success())
    {
        return;
    }
    let site = Site::start().await;
    let host = site.server.uri();
    site.server.reset().await;
    site.answer(
        "GET",
        "/app/api/projects",
        200,
        json!([{
            "id": site.project, "code": "sample", "status": "open", "repository_url": ORIGIN
        }]),
    )
    .await;
    site.answer("GET", &format!("/app/api/projects/{}", site.project), 200, json!({
        "repository_remotes": { "origin": ORIGIN, "forgejo": MIRROR, "forgejo_status": "verified" }
    })).await;
    site.answer("GET", "/app/api/firm-libraries", 200, json!([]))
        .await;

    let git = |dir: &Path, args: &[&str]| {
        let output = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let dir = site.dir.path();
    let seed = dir.join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "--initial-branch=main"]);
    std::fs::write(
        seed.join("navigator.yaml"),
        format!("project:\n  name: sample\n  host: {host}\n"),
    )
    .unwrap();
    git(&seed, &["add", "navigator.yaml"]);
    git(
        &seed,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "Source fixture",
        ],
    );
    for remote in ["origin.git", "mirror.git"] {
        git(dir, &["init", "--bare", "--initial-branch=main", remote]);
        git(&seed, &["push", dir.join(remote).to_str().unwrap(), "main"]);
    }
    let origin_rewrite = format!("url.{}.insteadOf", dir.join("origin.git").display());
    let mirror_rewrite = format!("url.{}.insteadOf", dir.join("mirror.git").display());
    let env = [
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", origin_rewrite.as_str()),
        ("GIT_CONFIG_VALUE_0", ORIGIN),
        ("GIT_CONFIG_KEY_1", mirror_rewrite.as_str()),
        ("GIT_CONFIG_VALUE_1", MIRROR),
    ];
    let sync = ["sync", "--host", &host, "--dir", "checkouts"];
    navigator_in(dir, &sync, &env).await.succeeded();
    let checkout = dir.join("checkouts/sample");
    assert!(checkout.join(".jj").is_dir());
    assert_eq!(
        git(&checkout, &["config", "--get", "remote.forgejo.url"]).trim(),
        MIRROR
    );
    let destinations = git(&checkout, &["config", "--get-all", "remote.origin.pushurl"]);
    assert_eq!(destinations.lines().collect::<Vec<_>>(), [ORIGIN, MIRROR]);
    assert_eq!(
        git(&checkout, &["rev-parse", "origin/main"]),
        git(&checkout, &["rev-parse", "forgejo/main"])
    );
    navigator_in(dir, &sync, &env).await.succeeded();
    assert_eq!(
        git(&checkout, &["config", "--get-all", "remote.origin.pushurl"]),
        destinations
    );
}
