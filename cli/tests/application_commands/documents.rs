//! `navigator inbox`, `navigator downloads`, and the `site document` read
//! verbs against a deployment: what each prints, writes, and refuses.

use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use crate::site::Site;

const MESSAGE: &str = "00000000-0000-0000-0000-000000000007";
const MATTER: &str = "00000000-0000-0000-0000-000000000003";
const LETTER: &[u8] = b"%PDF-1.4 synthetic engagement letter";

fn sha(bytes: &[u8]) -> String {
    store::documents::sha256_hex(bytes)
}

fn put(site: &Site, relative: &str, bytes: impl AsRef<[u8]>) -> std::path::PathBuf {
    let target = site.dir.path().join(relative);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, bytes).unwrap();
    target
}

/// Answer `verb route` with raw bytes.
async fn serve_bytes(site: &Site, verb: &str, route: &str, bytes: &[u8]) {
    Mock::given(method(verb))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .mount(&site.server)
        .await;
}

/// One inbox message's plan, already recorded and attributed to a matter.
fn plan() -> serde_json::Value {
    json!({
        "id": MESSAGE,
        "channel": "email",
        "provider_message_id": "<caf@example.test>",
        "external_address": "ecf@example.test",
        "subject": "Notice of Electronic Filing",
        "computed_route": "nef",
        "computed_label": "NEF",
        "reason": "the subject says so",
        "recorded_route": "nef",
        "recorded_at": "2026-10-31T12:00:00Z",
        "recorded_nef_outcome": "filed",
        "project_id": MATTER,
        "attachments": [{
            "asset_id": "00000000-0000-0000-0000-000000000009",
            "filename": "letter.pdf",
            "sha256": sha(LETTER),
        }],
    })
}

#[tokio::test]
async fn inbox_list_prints_the_backlog_says_it_is_empty_or_reports_the_refusal() {
    let backlog = "/app/api/inbox/unprocessed";
    let empty = Site::start().await;
    empty.answer("GET", backlog, 200, json!([])).await;
    let ran = empty.navigator(&["inbox", "list"]).await;
    assert!(ran.succeeded().contains("The inbox backlog is empty."));

    let waiting = Site::start().await;
    waiting.answer("GET", backlog, 200, json!([plan()])).await;
    let ran = waiting.navigator(&["inbox", "list"]).await;
    let row = format!(
        "{MESSAGE}\tNEF\tthe subject says so\tecf@example.test\tNotice of Electronic Filing\tfiled"
    );
    assert!(ran.succeeded().contains(&row), "{}", ran.stdout);

    let refusing = Site::start().await;
    let denied = json!({ "error": "denied" });
    refusing.answer("GET", backlog, 403, denied).await;
    let ran = refusing.navigator(&["inbox", "list"]).await;
    ran.refused(1, "inbox list refused: 403 Forbidden");
}

#[tokio::test]
async fn inbox_process_plans_dry_then_records_and_writes_the_attachments() {
    let site = Site::start().await;
    let door = "/app/api/inbox/process";
    let applied = json!({
        "result": plan(),
        "route": "nef",
        "route_label": "NEF",
        "recorded_at": "2026-10-31T12:00:00Z",
        "already_recorded": true,
        "slack_notice": "notified",
        "nef": { "outcome": "filed" },
        "attachments": { "filed": 1, "unfiled": 2 },
    });
    for (apply, body) in [(false, plan()), (true, applied)] {
        Mock::given(method("POST"))
            .and(path(door))
            .and(body_partial_json(json!({ "apply": apply })))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&site.server)
            .await;
    }
    serve_bytes(&site, "POST", "/app/api/inbox/attachment", LETTER).await;

    let dry = site
        .navigator(&["inbox", "process", MESSAGE, "--dir", "planned"])
        .await;
    let printed = dry.succeeded();
    for line in [
        "Provider id  <caf@example.test>".to_string(),
        format!("Matter       {MATTER}"),
        format!("Attachment   letter.pdf\tsha256:{}", sha(LETTER)),
        "Recorded     nef at 2026-10-31T12:00:00Z".to_string(),
        "Filing       filed".to_string(),
        "Dry run — nothing recorded. Next:".to_string(),
        format!("  navigator inbox process {MESSAGE} --apply --kind <kind>"),
    ] {
        assert!(printed.contains(&line), "{line}: {printed}");
    }
    assert!(!printed.contains("inbox attachments"), "{printed}");
    assert_eq!(
        std::fs::read(site.dir.path().join("planned/letter.pdf")).unwrap(),
        LETTER
    );

    let mut args = vec!["inbox", "process", MESSAGE, "--apply"];
    args.extend(["--kind", "exhibit", "--dir", "recorded"]);
    let recorded = site.navigator(&args).await;
    let printed = recorded.succeeded();
    for line in [
        "Recorded     NEF (nef) (already recorded — overwritten with the same truth)",
        "Slack notice notified",
        "Attachments  1 filed as internal documents",
        "Attachments  2 not filed; re-run with --apply --kind <kind> to file them",
        "No Gmail copy requested; nothing to archive.",
    ] {
        assert!(printed.contains(line), "{line}: {printed}");
    }
    assert!(site.dir.path().join("recorded/letter.pdf").is_file());
    let requests = site.server.received_requests().await.unwrap();
    let sent: Vec<serde_json::Value> = requests
        .iter()
        .filter(|request| request.url.path() == door)
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(
        sent,
        [
            json!({ "message": MESSAGE, "apply": false }),
            json!({ "message": MESSAGE, "apply": true, "kind": "exhibit" }),
        ]
    );
}

#[tokio::test]
async fn inbox_attachments_reports_a_refused_attachment_and_writes_nothing() {
    let site = Site::start().await;
    site.answer("POST", "/app/api/inbox/process", 200, plan())
        .await;
    let gone = json!({ "error": "gone" });
    site.answer("POST", "/app/api/inbox/attachment", 404, gone)
        .await;

    let ran = site
        .navigator(&["inbox", "attachments", MESSAGE, "--dir", "out"])
        .await;

    ran.refused(1, "inbox attachments refused: 404 Not Found");
    assert!(!site.dir.path().join("out").exists());
}

#[tokio::test]
async fn inbox_assign_matches_prints_each_decision_the_door_made() {
    let door = "/app/api/inbox/assign-matches";
    let site = Site::start().await;
    let view = |decision: serde_json::Value, applied: bool| {
        json!({
            "message": MESSAGE,
            "from": "client@example.com",
            "subject": "Retainer",
            "decision": decision,
            "applied": applied,
        })
    };
    let views = json!([
        view(json!({ "decision": "assigned" }), true),
        view(
            json!({ "decision": "proposed", "project_id": MATTER }),
            false
        ),
        view(json!({ "decision": "left_in_inbox" }), false),
        view(json!({ "decision": "held" }), false),
    ]);
    site.answer("POST", door, 200, views).await;
    let ran = site.navigator(&["inbox", "assign-matches"]).await;
    let printed = ran.succeeded();
    for line in [
        "From         client@example.com\nSubject      Retainer\n".to_string(),
        "Decision     assign to matter with no served project id\nApplied      assigned".into(),
        format!("Decision     propose assignment to matter {MATTER} (lawyer confirms)"),
        "Decision     left in the inbox\nApplied      proposed".to_string(),
        "Decision     held\n".to_string(),
        "Review each proposal and confirm the filing individually.".to_string(),
    ] {
        assert!(printed.contains(&line), "{line}: {printed}");
    }

    let refusing = Site::start().await;
    refusing.answer("POST", door, 500, json!({})).await;
    let ran = refusing.navigator(&["inbox", "assign-matches"]).await;
    ran.refused(1, "inbox assign-matches refused: 500");

    let stuck = Site::start().await;
    Mock::given(method("POST"))
        .and(path(door))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-navigator-next-cursor", "[\"2026-01-01T00:00:00Z\"]")
                .set_body_json(json!([])),
        )
        .mount(&stuck.server)
        .await;
    let ran = stuck
        .navigator(&["inbox", "assign-matches", "--scheduled"])
        .await;
    ran.refused(1, "assign-matches continuation did not advance");
    assert!(ran.stdout.contains("Continuing with the next inbox batch."));
}

#[tokio::test]
async fn inbox_import_verbs_refuse_bad_input_before_reaching_the_deployment() {
    let site = Site::start().await;
    let mut import = vec!["inbox", "import", "--source", "carrier-pigeon"];
    import.extend(["--mailbox", "lawyer@example.com"]);
    site.navigator(&import)
        .await
        .refused(1, "unknown --source `carrier-pigeon`");

    let mut fi = vec!["inbox", "import-fi", "--file", "absent.csv"];
    fi.extend(["--mapping", "absent.json", "--account", "+17025550100"]);
    fi.extend(["--brand", "sample"]);
    site.navigator(&fi).await.refused(1, "navigator inbox:");
    assert!(site.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn downloads_scan_reports_each_files_intake_facts() {
    let site = Site::start().await;
    std::fs::create_dir_all(site.dir.path().join("Downloads/nested")).unwrap();
    let empty = site.navigator(&["downloads", "scan"]).await;
    assert!(empty
        .succeeded()
        .contains("The Downloads folder holds no candidates."));

    put(&site, "Downloads/sample-notice.pdf", b"notice");
    put(&site, "Downloads/firm-api.pem", b"synthetic");
    let rows = site.navigator(&["downloads", "scan"]).await;
    let notice = format!(
        "sample-notice.pdf\t{}\tapplication/pdf\t6\t0\tunfiled\t",
        sha(b"notice")
    );
    assert!(rows.succeeded().contains(&notice), "{}", rows.stdout);
    assert!(rows.stdout.contains("\tcredential\t"), "{}", rows.stdout);

    let as_json = site.navigator(&["downloads", "scan", "--json"]).await;
    let facts: serde_json::Value = serde_json::from_str(as_json.succeeded()).unwrap();
    assert_eq!(facts[0]["filename"], "firm-api.pem");
    assert_eq!(facts[0]["class"], "credential");
    assert_eq!(facts[1]["size_bytes"], 6);

    let later = ["downloads", "scan", "--since", "2999-01-01"];
    let none = site.navigator(&later).await;
    assert!(none.succeeded().contains("holds no candidates"));
    site.navigator(&["downloads", "scan", "--since", "tomorrow"])
        .await
        .refused(1, "--since must be an ISO date, YYYY-MM-DD");
}

#[tokio::test]
async fn downloads_propose_names_the_matter_kind_and_slug_with_its_evidence() {
    let site = Site::start().await;
    for name in [
        "sample-notice.pdf",
        "photo.png",
        "memo.md",
        "!!!.pdf",
        "2026.10.12_filing-receipt.pdf",
    ] {
        put(&site, &format!("inbound/{name}"), name);
    }
    let propose = ["downloads", "propose", "--dir", "inbound"];

    let ran = site.navigator(&propose).await;
    let printed = ran.succeeded();
    for line in [
        "Matter       sample\n",
        "the filename names matter code `sample`",
        "Kind         exhibit\n",
        "an image files as an exhibit",
        "markdown work product — the filename convention does not govern it",
        "no date names this file — pass --slug when filing",
        "Class        receipt (the filename says receipt)",
        "Kind         invoice\n",
        "Slug         2026.10.12_filing-receipt.pdf\n",
        "no visible matter's code appears in the filename or provenance",
    ] {
        assert!(printed.contains(line), "{line}: {printed}");
    }

    let as_json = site.navigator(&[&propose[..], &["--json"]].concat()).await;
    let proposals: serde_json::Value = serde_json::from_str(as_json.succeeded()).unwrap();
    let named: Vec<&serde_json::Value> = proposals
        .as_array()
        .unwrap()
        .iter()
        .filter(|proposal| proposal["project"] == "sample")
        .collect();
    assert_eq!(named.len(), 1, "{proposals}");
    assert_eq!(named[0]["filename"], "sample-notice.pdf");

    let signed_out = Site::signed_out().await;
    put(&signed_out, "inbound/sample-notice.pdf", b"notice");
    let ran = signed_out.navigator(&propose).await;
    let printed = ran.succeeded();
    assert!(
        printed.contains("no matter proposals without a login:"),
        "{printed}"
    );
    assert!(
        printed.contains("no visible matter names a code in this candidate"),
        "{printed}"
    );
    assert!(printed.contains("Matter       -\n"), "{printed}");
}

/// The pointer the document door answers an upload of `bytes` with.
fn receipt(bytes: &[u8]) -> serde_json::Value {
    json!({
        "kind": "filing",
        "visibility": "internal",
        "current_version": {
            "version": 1,
            "asset_id": Uuid::now_v7(),
            "created_at": "2026-10-12T12:00:00Z",
            "sha256": sha(bytes),
            "size_bytes": bytes.len(),
        },
    })
}

#[tokio::test]
async fn downloads_file_moves_the_original_only_after_the_receipt_matches() {
    let name = "2026.10.12_complaint.pdf";
    let mut file = vec!["downloads", "file", "inbound/2026.10.12_complaint.pdf"];
    file.extend(["--project", "sample", "--kind", "filing"]);

    let site = Site::start().await;
    let original = put(&site, &format!("inbound/{name}"), b"complaint");
    site.answer("POST", &site.door("documents"), 201, receipt(b"complaint"))
        .await;
    let ran = site.navigator(&file).await;
    let printed = ran.succeeded();
    assert!(
        printed.contains(&format!("onto sample as kind `filing`, slug `{name}`.")),
        "{printed}"
    );
    assert!(
        printed.contains(&format!("sha256 verified: {}", sha(b"complaint"))),
        "{printed}"
    );
    assert!(!original.exists(), "the original moved to the Trash");
    let trashed = walk(site.dir.path())
        .into_iter()
        .filter(|found| found.ends_with(name))
        .collect::<Vec<_>>();
    assert_eq!(trashed.len(), 1, "{trashed:?}");
    assert_eq!(std::fs::read(&trashed[0]).unwrap(), b"complaint");

    let mismatched = Site::start().await;
    let original = put(&mismatched, &format!("inbound/{name}"), b"complaint");
    let other = receipt(b"other bytes");
    mismatched
        .answer("POST", &mismatched.door("documents"), 201, other)
        .await;
    mismatched
        .navigator(&file)
        .await
        .refused(1, "the original was NOT moved to the Trash");
    assert!(original.exists());

    put(&mismatched, "inbound/firm-api.pem", b"synthetic");
    let mut credential = vec!["downloads", "file", "inbound/firm-api.pem"];
    credential.extend(["--project", "sample", "--kind", "filing"]);
    mismatched
        .navigator(&credential)
        .await
        .refused(1, "A credential belongs in the keychain, never on a matter");
}

/// Every regular file below `root`.
fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap().path();
            if entry.is_dir() {
                pending.push(entry);
            } else {
                found.push(entry);
            }
        }
    }
    found
}

#[tokio::test]
async fn downloads_clean_joins_candidates_to_filed_documents_by_digest() {
    let site = Site::start().await;
    let filed = |bytes: &[u8], filename: Option<&str>, kind: Option<&str>| {
        json!({
            "id": Uuid::now_v7(),
            "byte_size": bytes.len(),
            "sha256_hex": sha(bytes),
            "filename": filename,
            "kind": kind,
            "visibility": "internal",
            "slug": filename,
            "inserted_at": "2026-10-12T12:00:00Z",
        })
    };
    let inventory = json!([
        filed(b"one", Some("one-filed.pdf"), Some("filing")),
        filed(b"two", Some("two-filed.pdf"), None),
        filed(b"three", None, None),
    ]);
    site.answer("GET", &site.door("documents"), 200, inventory)
        .await;
    for name in ["one", "two", "three", "four"] {
        put(&site, &format!("inbound/{name}.pdf"), name);
    }
    let clean = ["downloads", "clean", "--dir", "inbound"];
    site.navigator(&clean)
        .await
        .refused(1, "pass --already-filed");

    let confirmed = [&clean[..], &["--already-filed"]].concat();
    let ran = site.navigator(&confirmed).await;
    let printed = ran.succeeded();
    for line in [
        "4 candidates, 3 already filed on a visible matter.",
        "one.pdf` — already filed on sample as `one-filed.pdf` (kind `filing`)",
        "two.pdf` — already filed on sample as `two-filed.pdf`\n",
        "three.pdf` — already filed on sample (digest matches, kind unknown)",
        "four.pdf` — not filed on any visible matter",
    ] {
        assert!(printed.contains(line), "{line}: {printed}");
    }

    let elsewhere = [&confirmed[..], &["--project", "sample-estate"]].concat();
    let ran = site.navigator(&elsewhere).await;
    assert!(
        ran.succeeded().contains("4 candidates, 0 already filed"),
        "{}",
        ran.stdout
    );

    let unreadable = Site::start().await;
    put(&unreadable, "inbound/one.pdf", b"one");
    unreadable
        .answer("GET", &unreadable.door("documents"), 500, json!({}))
        .await;
    let ran = unreadable.navigator(&confirmed).await;
    assert!(ran.succeeded().contains("1 candidates, 0 already filed"));
    assert!(
        ran.stderr.contains("could not read matter sample:"),
        "{}",
        ran.stderr
    );
}

const FIRST: &[u8] = b"alpha\nbeta\ncharlie\n";
const SECOND: &[u8] = b"alpha\nbravo\ncharlie\ndelta\n";

fn revision(version: usize, asset: Uuid, bytes: &[u8], operative: bool) -> serde_json::Value {
    json!({
        "version": version,
        "asset_id": asset,
        "created_at": "2026-09-06T00:00:00Z",
        "sha256": sha(bytes),
        "size_bytes": bytes.len(),
        "filename": "notes.txt",
        "visibility": "internal",
        "operative": operative,
    })
}

fn pointer(asset: Uuid, bytes: &[u8]) -> String {
    serde_yaml::to_string(&json!({
        "kind": "memo",
        "visibility": "internal",
        "previous_version": Uuid::now_v7(),
        "current_version": {
            "version": 2,
            "asset_id": asset,
            "created_at": "2026-09-06T00:00:00Z",
            "sha256": sha(bytes),
            "size_bytes": bytes.len(),
        },
    }))
    .unwrap()
}

/// A checkout of the `sample` Project whose manifest names the deployment.
async fn checkout(project: &str) -> Site {
    let site = Site::start().await;
    let manifest = format!("project: {project}\nhost: {}\n", site.server.uri());
    put(&site, "navigator.yaml", manifest);
    site
}

async fn serve_chain(site: &Site, slug: &str, revisions: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(site.door("documents/revisions")))
        .and(query_param("slug", slug))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "kind": "memo", "revisions": revisions })),
        )
        .mount(&site.server)
        .await;
}

fn download(asset: Uuid) -> String {
    format!("/app/projects/sample/documents/{asset}/download")
}

#[tokio::test]
async fn document_log_get_and_diff_read_the_revision_chain_a_pointer_names() {
    let site = checkout("sample").await;
    let (first, second) = (Uuid::now_v7(), Uuid::now_v7());
    let blob = "blobs/memos/notes.txt.yaml";
    put(&site, blob, pointer(second, SECOND));
    let chain = json!([
        revision(2, second, SECOND, true),
        revision(1, first, FIRST, false),
    ]);
    serve_chain(&site, "memos/notes.txt", chain).await;
    serve_chain(&site, "memos/unfiled.txt", json!([])).await;
    serve_bytes(&site, "GET", &download(first), FIRST).await;
    serve_bytes(&site, "GET", &download(second), SECOND).await;
    let document = ["site", "document"];

    let log = site
        .navigator(&[&document[..], &["log", blob]].concat())
        .await;
    let printed = log.succeeded();
    assert!(printed.contains("memos/notes.txt (memo)"), "{printed}");
    assert!(printed.contains("notes.txt"), "{printed}");
    let unfiled = [&document[..], &["log", "documents/memos/unfiled.txt"]].concat();
    let ran = site.navigator(&unfiled).await;
    assert!(
        ran.succeeded()
            .contains("no revision of `memos/unfiled.txt` is visible under your lens"),
        "{}",
        ran.stdout
    );

    let get = [&document[..], &["get", blob, "--out"]].concat();
    let ran = site
        .navigator(&[&get[..], &["operative.txt"]].concat())
        .await;
    assert!(
        ran.succeeded().contains("wrote revision 2 (26 bytes"),
        "{}",
        ran.stdout
    );
    assert_eq!(
        std::fs::read(site.dir.path().join("operative.txt")).unwrap(),
        SECOND
    );
    let earlier = [&get[..], &["earlier.txt", "--version", "1"]].concat();
    site.navigator(&earlier).await.succeeded();
    assert_eq!(
        std::fs::read(site.dir.path().join("earlier.txt")).unwrap(),
        FIRST
    );
    let absent = [&get[..], &["absent.txt", "--version", "9"]].concat();
    site.navigator(&absent).await.refused(
        2,
        "version 9 is not visible under your lens (available: 1..=2)",
    );
    let into_blobs = [&get[..], &["blobs/memos/copy.txt"]].concat();
    site.navigator(&into_blobs)
        .await
        .refused(2, "it holds only committed pointers, never fetched bytes");

    let diff = [&document[..], &["diff", blob, "1", "2"]].concat();
    let ran = site.navigator(&diff).await;
    let redline = "--- version 1 (notes.txt)\n+++ version 2 (notes.txt)\n  alpha\n- beta\n+ bravo\n  charlie\n+ delta\n";
    assert_eq!(ran.succeeded(), redline);
}

#[tokio::test]
async fn document_get_refuses_bytes_that_miss_the_recorded_digest_or_size() {
    let site = checkout("sample").await;
    let (first, second, third) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let blob = "blobs/memos/notes.txt.yaml";
    put(&site, blob, pointer(third, SECOND));
    let mut short = revision(1, first, FIRST, false);
    short["size_bytes"] = json!(3);
    let chain = json!([
        revision(3, third, SECOND, true),
        revision(2, second, SECOND, false),
        short,
    ]);
    serve_chain(&site, "memos/notes.txt", chain).await;
    serve_bytes(&site, "GET", &download(first), FIRST).await;
    serve_bytes(&site, "GET", &download(second), FIRST).await;
    let get = ["site", "document", "get", blob, "--out", "fetched.txt"];

    let ran = site
        .navigator(&[&get[..], &["--version", "2"]].concat())
        .await;
    ran.refused(2, &format!("sha256 mismatch on revision {second}"));
    let ran = site
        .navigator(&[&get[..], &["--version", "1"]].concat())
        .await;
    ran.refused(
        2,
        &format!("size mismatch on revision {first}: expected 3 bytes, got 19"),
    );
    assert!(!site.dir.path().join("fetched.txt").exists());

    put(&site, blob, pointer(Uuid::now_v7(), SECOND));
    let log = ["site", "document", "log", blob];
    site.navigator(&log)
        .await
        .refused(2, "which is not in the live chain");
}

fn authority_pointer(authority: Uuid, bytes: &[u8]) -> String {
    let mut value: serde_json::Value =
        serde_yaml::from_str(&pointer(Uuid::now_v7(), bytes)).unwrap();
    value["kind"] = json!("");
    value["authority_id"] = json!(authority);
    serde_yaml::to_string(&value).unwrap()
}

#[tokio::test]
async fn validate_checks_each_committed_pointer_against_the_deployment() {
    let site = checkout("sample").await;
    let (asset, authority) = (Uuid::now_v7(), Uuid::now_v7());
    put(&site, "blobs/memos/notes.txt.yaml", pointer(asset, SECOND));
    put(
        &site,
        "blobs/cases/opinion.html.yaml",
        authority_pointer(authority, FIRST),
    );
    let chain = json!([revision(2, asset, SECOND, true)]);
    serve_chain(&site, "memos/notes.txt", chain).await;
    let archive = format!("/app/api/authorities/{authority}/archive");
    serve_bytes(&site, "GET", &archive, FIRST).await;

    let ran = site.navigator(&["validate", "."]).await;
    assert!(
        ran.stdout
            .contains("Verified 2 document pointer(s) live, found 0 error(s)"),
        "{}{}",
        ran.stdout,
        ran.stderr
    );

    let drifted = checkout("sample").await;
    put(
        &drifted,
        "blobs/memos/notes.txt.yaml",
        pointer(asset, FIRST),
    );
    put(
        &drifted,
        "blobs/cases/opinion.html.yaml",
        authority_pointer(authority, SECOND),
    );
    let chain = json!([revision(2, asset, SECOND, true)]);
    serve_chain(&drifted, "memos/notes.txt", chain).await;
    serve_bytes(&drifted, "GET", &archive, FIRST).await;
    let ran = drifted.navigator(&["validate", "."]).await;
    assert!(
        ran.stdout.contains("live, found 2 error(s)"),
        "{}",
        ran.stdout
    );
    for reason in [
        "live document verification failed: drift: sha256 mismatch",
        "drift: Authority archive sha256 or size differs from pointer",
        "2 live document pointer check(s) failed",
    ] {
        assert!(ran.stderr.contains(reason), "{reason}: {}", ran.stderr);
    }
    assert_eq!(ran.code, Some(1));
}

#[tokio::test]
async fn validate_skips_the_live_check_without_a_session_for_the_manifest_host() {
    let blob = "blobs/memos/notes.txt.yaml";
    let skipped = "Live document verification skipped for 1 pointer(s): ";
    let expired = |host: &str| {
        let lapsed = json!({ "token": "navigator-session", "role": "lawyer", "expires_at": 1 });
        json!({ "hosts": { host: lapsed } }).to_string()
    };
    let cases: [(&str, Option<&str>, &str); 3] = [
        ("project: sample\n", None, "navigator.yaml names no host"),
        (
            "project: sample\nhost: https://elsewhere.example.test\n",
            None,
            "no stored session for the manifest host",
        ),
        (
            "project: sample\nhost: https://elsewhere.example.test\n",
            Some("https://elsewhere.example.test"),
            "stored session for the manifest host has expired",
        ),
    ];
    for (manifest, lapsed, reason) in cases {
        let site = Site::start().await;
        put(&site, "navigator.yaml", manifest);
        put(&site, blob, pointer(Uuid::now_v7(), SECOND));
        if let Some(host) = lapsed {
            put(&site, "credentials.json", expired(host));
        }
        let ran = site.navigator(&["validate", "."]).await;
        assert!(
            ran.stdout.contains(&format!("{skipped}{reason}")),
            "{reason}: {}{}",
            ran.stdout,
            ran.stderr
        );
    }

    let hidden = checkout("sample-estate").await;
    put(&hidden, blob, pointer(Uuid::now_v7(), SECOND));
    let ran = hidden.navigator(&["validate", "."]).await;
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    for reason in [
        "notes.txt.yaml: live document verification failed: no visible matter",
        "live document verification could not read the Project",
    ] {
        assert!(ran.stderr.contains(reason), "{reason}: {}", ran.stderr);
    }
}
